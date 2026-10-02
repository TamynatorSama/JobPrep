"""Bridge auth + config endpoints for the browser extension.

  POST /config/seed   {llm: {...}}     -> refresh the in-memory LLM config
  GET  /config/ping                    -> connectivity + auth check for the popup

Both require the shared secret in the ``X-InterPrep-Token`` header. The token is
established only from the env var set by the Rust shell (see runtime_config); it
can never be set over HTTP, so a malicious page can't authenticate itself.
"""
from __future__ import annotations

import asyncio

from fastapi import APIRouter, Depends, Header, HTTPException

import runtime_config
from models import LLMConfig, SeedConfigRequest

router = APIRouter()

# The route last warmed (provider, model). A seed warms again only when the
# copilot's route changed (Settings save), not on every token refresh.
_warmed: tuple | None = None


async def _warm_llm_channel(target: LLMConfig | None) -> None:
    """Fire tiny generations so the FIRST real answer doesn't pay the provider
    cold path (client construction + TLS/HTTP2 channel + session setup —
    measured ~70s worst-case on a cold boot when it also contends with the
    package pre-import, ~2-3s otherwise; a first screen answer on the ChatGPT
    plan took 21 s when the wrong route had been warmed). `target` is the
    copilot's route (Rust sends it with the seed); older callers fall back to
    the seeded extension config. Runs in the background; costs a handful of
    tokens."""
    global _warmed
    cfg = target or runtime_config.get_llm_config()
    import llm_provider as llm_factory
    if llm_factory.missing_key_error(cfg):
        return  # no key yet — the next seed (Settings save) retries
    key = (cfg.provider, cfg.model)
    if _warmed == key:
        return
    _warmed = key
    try:
        import time
        from routes.chat import COPILOT_MAX_TOKENS, SCREEN_MAX_TOKENS
        llm_factory.set_feature("warmup")
        t0 = time.time()
        # Same tier/temperature/max_tokens as a copilot answer, so this builds
        # and caches the EXACT client the first live question will reuse (the
        # factory caches models by those params) — connection already open.
        await llm_factory.generate_raw(cfg, "Reply with the single word: ok",
                                       tier="instant", temperature=None,
                                       max_tokens=COPILOT_MAX_TOKENS)
        print(f"[bridge] LLM channel warmed in {time.time() - t0:.1f}s "
              f"({cfg.provider} {cfg.model or 'auto'})", flush=True)
        # ...and the client a screen-capture answer uses (fast tier).
        await llm_factory.generate_raw(cfg, "Reply with the single word: ok",
                                       tier="fast", temperature=None,
                                       max_tokens=SCREEN_MAX_TOKENS)
    except Exception as exc:  # warmup is best-effort, never a failure surface
        _warmed = None
        print(f"[bridge] LLM warmup failed (will retry on next seed): {exc}", flush=True)


async def require_token(x_interprep_token: str = Header(default="")) -> None:
    """FastAPI dependency: 401 unless the request carries the shared secret.

    Constant-time-ish compare isn't critical here (localhost, no timing oracle
    over loopback), but we still reject empty/mismatched tokens outright."""
    expected = runtime_config.get_token()
    if not expected or x_interprep_token != expected:
        raise HTTPException(status_code=401, detail="invalid or missing bridge token")


@router.post("/seed")
async def seed(req: SeedConfigRequest, _: None = Depends(require_token)):
    runtime_config.set_llm_config(req.llm)
    # Warm the provider channel in the background so the first real question
    # (copilot / coach / interview) streams immediately instead of paying the
    # cold path. Fire-and-forget; /config/seed must stay fast for the caller.
    asyncio.get_running_loop().create_task(_warm_llm_channel(req.warm))
    return {"ok": True}


@router.get("/ping")
async def ping(_: None = Depends(require_token)):
    import llm_provider as llm_factory
    cfg = runtime_config.get_llm_config()
    return {"ok": True, "has_key": llm_factory.missing_key_error(cfg) is None}
