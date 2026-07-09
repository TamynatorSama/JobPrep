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
from models import SeedConfigRequest

router = APIRouter()

# One-shot guard for the LLM channel warmup below.
_warmed = False


async def _warm_llm_channel() -> None:
    """Fire one tiny fast-tier generation so the FIRST real question doesn't pay
    the provider cold path (client construction + TLS/HTTP2 channel + session
    setup — measured ~70s worst-case on a cold boot when it also contends with
    the package pre-import, ~2-3s otherwise). Runs in the background right after
    the Rust shell seeds credentials at startup; costs a handful of tokens."""
    global _warmed
    if _warmed:
        return
    cfg = runtime_config.get_llm_config()
    import llm_provider as llm_factory
    if llm_factory.missing_key_error(cfg):
        return  # no key yet — the next seed (Settings save) retries
    _warmed = True
    try:
        import time
        t0 = time.time()
        await llm_factory.generate_raw(cfg, "Reply with the single word: ok",
                                       tier="fast", temperature=0.0)
        print(f"[bridge] LLM channel warmed in {time.time() - t0:.1f}s", flush=True)
    except Exception as exc:  # warmup is best-effort, never a failure surface
        _warmed = False
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
    asyncio.get_running_loop().create_task(_warm_llm_channel())
    return {"ok": True}


@router.get("/ping")
async def ping(_: None = Depends(require_token)):
    import llm_provider as llm_factory
    cfg = runtime_config.get_llm_config()
    return {"ok": True, "has_key": llm_factory.missing_key_error(cfg) is None}
