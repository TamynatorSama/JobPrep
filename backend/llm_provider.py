"""Multi-provider chat-model factory.

Every route builds its LLM through here so the provider the user picks in
Settings (gemini | openai | anthropic) applies app-wide.
Requests carry an `LLMConfig` (see models.py) with the selected provider and
the keys for every provider the user has configured — the factory uses the
selected one; company research additionally uses the spare keys as fallback
lanes (see routes/company_research.py).

Partner packages are imported lazily so a missing optional dependency only
breaks the provider that needs it, not the whole sidecar.
"""
from __future__ import annotations

import contextvars
import hashlib
import json
import re
import threading
import time

from langchain_core.messages import HumanMessage

from models import LLMConfig

PROVIDER_LABELS = {
    "gemini": "Gemini",
    "openai": "OpenAI",
    "chatgpt": "ChatGPT plan",
    "anthropic": "Anthropic (Claude)",
}

# ── Model registry ───────────────────────────────────────────────────────────
# Candidate models per provider+tier, preferred-first. Routes walk the list so a
# model that 404s / is access-restricted on the user's account falls through to
# the next. Refreshed 2026-10-01 against the providers' model pages:
#   * Gemini 2.5 is now access-restricted to accounts that already used it, so
#     a NEW key can't reach it — 3.x leads, 2.5 stays last for older accounts.
#   * Claude Haiku 4.5 retires no sooner than 2026-10-15; Sonnet 5.5 backs it.
#   * OpenAI's latency-tier pick is the gpt-5.6 family (luna/terra/sol).
#
# "instant" = live copilot answers: lowest time-to-first-token, minimal thinking;
# "fast"    = coach chat, mock interviewer, utility calls (summaries, routers);
# "mid"     = cost/quality middle ground the resume orchestrator routes easy/
#             medium jobs to (see model_router.py);
# "smart"   = hard structured work (resume tailoring, cheatsheet, extension).
_MODELS: dict[str, dict[str, list[str]]] = {
    "gemini": {
        "instant": ["gemini-3.5-flash-lite", "gemini-3.1-flash-lite", "gemini-2.5-flash"],
        "fast":    ["gemini-3.8-flash", "gemini-3.5-flash", "gemini-2.5-flash"],
        "mid":     ["gemini-3.8-flash", "gemini-2.5-pro", "gemini-2.5-flash"],
        "smart":   ["gemini-3.1-pro-preview", "gemini-3.8-flash", "gemini-2.5-pro"],
    },
    "openai": {
        "instant": ["gpt-5.6-luna", "gpt-4o-mini"],
        "fast":    ["gpt-5.6-luna", "gpt-4o-mini"],
        "mid":     ["gpt-5.6-terra", "gpt-4.1"],
        "smart":   ["gpt-5.6-sol", "gpt-5", "gpt-4.1"],
    },
    "anthropic": {
        "instant": ["claude-haiku-4-5", "claude-sonnet-5-5"],
        "fast":    ["claude-haiku-4-5", "claude-sonnet-5-5"],
        "mid":     ["claude-sonnet-5-5", "claude-sonnet-4-6"],
        "smart":   ["claude-opus-5-5", "claude-sonnet-5-5"],
    },
}

# Tiers where latency beats deliberation: thinking is dialed to its minimum.
_LOW_LATENCY_TIERS = ("instant", "fast")

# OpenAI reasoning-family models reject explicit temperature (must use the
# model default) — skip the param rather than burn a failed call.
_OPENAI_NO_TEMPERATURE = ("gpt-5", "o1", "o3", "o4")


def _gemini_thinking(name: str, tier: str) -> dict:
    """Per-model thinking control. Gemini 2.5 takes a token budget (0 = off);
    Gemini 3.x takes a level and can't fully disable thinking — Flash-Lite goes
    down to "minimal", Flash to "low". Deeper tiers keep the model default.
    Keyed on the model family, NOT a single substring, so swapping a model in
    the registry can't silently fall back to slow default thinking."""
    if tier not in _LOW_LATENCY_TIERS:
        return {}
    if name.startswith("gemini-2.5"):
        return {"thinking_budget": 0}
    if name.startswith("gemini-3") and "pro" not in name:
        if "flash-lite" in name:
            return {"thinking_level": "minimal" if tier == "instant" else "low"}
        if "flash" in name:
            return {"thinking_level": "low"}
    return {}


def default_model(provider: str, tier: str = "fast", cfg: LLMConfig | None = None) -> str:
    """First registry candidate for a provider/tier (no user override)."""
    return _tier_models(provider, tier, cfg)[0]


# ── ChatGPT plan models ──────────────────────────────────────────────────────
# Sign in with ChatGPT (provider "chatgpt") can only call the models the user's
# plan exposes, so its "registry" is fetched: GET /v1/models with the plan
# token returns {models: [{slug, visibility}]}; keep visibility "list", in the
# server's order. Cached an hour (one user; tokens rotate hourly).
_PLAN_MODELS_URL = "https://api.openai.com/v1/models"
_PLAN_FALLBACK = ["gpt-6.1-sol"]   # the docs' example model, if listing fails
_plan_models: tuple[float, list[str]] | None = None


def chatgpt_plan_models(token: str) -> list[str]:
    global _plan_models
    if _plan_models and time.time() - _plan_models[0] < 3600:
        return _plan_models[1]
    models: list[str] = []
    try:
        import httpx
        r = httpx.get(_PLAN_MODELS_URL, headers={"Authorization": f"Bearer {token}"}, timeout=10)
        r.raise_for_status()
        body = r.json()
        for m in body.get("models") or body.get("data") or []:
            slug = m.get("slug") or m.get("id")
            if slug and m.get("visibility", "list") == "list":
                models.append(slug)
    except Exception as exc:  # noqa: BLE001 — fall back to the documented model
        print(f"[chatgpt] model list failed: {type(exc).__name__}", flush=True)
    if not models:
        # Don't pin a failure for an hour: retry within a minute.
        _plan_models = (time.time() - 3540, list(_PLAN_FALLBACK))
        return list(_PLAN_FALLBACK)
    _plan_models = (time.time(), models)
    return models


def _plan_tiers(models: list[str]) -> dict[str, list[str]]:
    """Map plan models onto tiers by name: light (luna/mini/nano/lite) serves
    instant/fast, mid (terra) serves mid, the rest (sol/flagship) serves smart;
    each tier falls back to whatever exists."""
    light = [m for m in models if any(k in m for k in ("luna", "mini", "nano", "lite"))]
    mid = [m for m in models if "terra" in m and m not in light]
    heavy = [m for m in models if m not in light and m not in mid]
    first = lambda *groups: next((g for g in groups if g), models)[:2]  # noqa: E731
    return {
        "instant": first(light, mid, heavy),
        "fast": first(light, mid, heavy),
        "mid": first(mid, heavy, light),
        "smart": first(heavy, mid, light),
    }


def _tier_models(provider: str, tier: str, cfg: LLMConfig | None = None) -> list[str]:
    """A provider's registry models for a tier (plan models for "chatgpt")."""
    if provider == "chatgpt":
        token = cfg.chatgpt_access_token if cfg else ""
        tiers = _plan_tiers(chatgpt_plan_models(token) if token else list(_PLAN_FALLBACK))
    else:
        tiers = _MODELS.get(provider) or _MODELS["gemini"]
    return list(tiers.get(tier) or tiers["fast"])


def provider_of(cfg: LLMConfig) -> str:
    return (cfg.provider or "gemini").strip().lower()


def api_key_for(cfg: LLMConfig, provider: str | None = None) -> str:
    """The API key for `provider` (default: the selected provider)."""
    return {
        "gemini": cfg.gemini_api_key,
        "openai": cfg.openai_api_key,
        "chatgpt": cfg.chatgpt_access_token,
        "anthropic": cfg.anthropic_api_key,
    }.get(provider or provider_of(cfg), "")


def missing_key_error(cfg: LLMConfig) -> str | None:
    """User-facing error when the selected provider isn't usable, else None."""
    p = provider_of(cfg)
    if p not in PROVIDER_LABELS:
        return f"Unknown AI provider `{p}`. Open **Settings → API Keys**."
    if p == "chatgpt" and not cfg.chatgpt_access_token:
        return ("Not signed in with ChatGPT (or the session expired). "
                "Open **Settings → API Keys** and sign in again, or pick another provider.")
    if not api_key_for(cfg):
        return (
            f"No {PROVIDER_LABELS[p]} API key set. "
            "Open **Settings → API Keys** and paste one (or switch provider)."
        )
    return None


def candidate_models(cfg: LLMConfig, tier: str = "fast") -> list[str]:
    """Models to try in order for the selected provider. A user override from
    Settings replaces the whole list."""
    if cfg.model.strip():
        return [cfg.model.strip()]
    return _tier_models(provider_of(cfg), tier, cfg)


def routes_for(cfg: LLMConfig, tier: str = "fast") -> list[tuple[str, str]]:
    """(provider, model) pairs to try in order: the selected provider's
    candidates, then each configured fallback provider's (its pinned model, or
    its registry models for this tier). Fallbacks without a key are skipped."""
    primary = provider_of(cfg)
    out = [(primary, m) for m in candidate_models(cfg, tier)]
    for fb in cfg.fallbacks:
        p = (fb.provider or "").strip().lower()
        if p == primary or p not in PROVIDER_LABELS or not api_key_for(cfg, p):
            continue
        models = [fb.model.strip()] if fb.model.strip() else _tier_models(p, tier, cfg)
        out += [(p, m) for m in models if (p, m) not in out]
    return out


def route_event(cfg: LLMConfig, provider: str, model: str) -> dict | None:
    """SSE `route` event for the UI's badge when the answer did NOT come from
    the user's chosen provider (a fallback kicked in, or the chosen provider
    can't serve this feature). None when it's the expected provider."""
    primary = provider_of(cfg)
    fallback = provider != primary
    if not fallback and not cfg.substituted_from:
        return None
    # The provider the user actually picked: the stand-in's original choice, or
    # the primary that failed over.
    chosen = cfg.substituted_from or primary
    return {"type": "route", "content": {
        "provider": provider,
        "providerLabel": PROVIDER_LABELS.get(provider, provider),
        "model": model,
        "fallback": fallback,
        "substitutedFrom": cfg.substituted_from,
        "chosenLabel": PROVIDER_LABELS.get(chosen, chosen),
        "feature": cfg.feature,
    }}


# Built models are cached so repeated calls REUSE the provider client and its
# open HTTP/TLS connection — a new client per call paid connection setup on
# every copilot question, and made the /config/seed channel warmup pointless.
# LangChain chat models are stateless config + a client, safe to share across
# concurrent calls. Keyed by everything that shapes the model (the API key only
# as a hash). Tiny bound: a session uses a handful of combinations.
_model_cache: dict[tuple, object] = {}
_model_cache_lock = threading.Lock()
_MODEL_CACHE_MAX = 32


def make_chat_model(
    cfg: LLMConfig,
    model_name: str | None = None,
    *,
    tier: str = "fast",
    temperature: float | None = None,
    max_tokens: int | None = None,
    provider: str | None = None,
):
    """Construct (or reuse) a LangChain chat model.

    `provider=None` uses the selected provider; a fallback route passes its
    own. `model_name=None` uses the first candidate of `tier`. `max_tokens`
    caps the output (a runaway guard for short-answer paths like the copilot;
    None keeps the provider default). All returned models support `.ainvoke`
    and `.astream` with langchain-core message types.
    """
    p = provider or provider_of(cfg)
    if model_name:
        name = model_name
    elif p == provider_of(cfg):
        name = candidate_models(cfg, tier)[0]
    else:
        name = default_model(p, tier, cfg)
    key = api_key_for(cfg, p)
    cache_key = (
        p, name, tier, temperature, max_tokens,
        hashlib.sha256(key.encode("utf-8")).hexdigest()[:16],
    )
    with _model_cache_lock:
        cached = _model_cache.get(cache_key)
    if cached is not None:
        return cached
    model = _build_chat_model(p, name, key, tier, temperature, max_tokens)
    with _model_cache_lock:
        if len(_model_cache) >= _MODEL_CACHE_MAX:
            _model_cache.pop(next(iter(_model_cache)))
        _model_cache[cache_key] = model
    return model


def _build_chat_model(p, name, key, tier, temperature, max_tokens):
    # LangChain clients default to ~6 retries with exponential backoff, which
    # turns a dead key / down model into a minute-long stall before our own
    # candidate-fallback can act. One retry is enough for a transient blip;
    # anything persistent should fail fast so the next candidate gets a shot.
    if p == "gemini":
        from langchain_google_genai import ChatGoogleGenerativeAI
        kwargs = {} if temperature is None else {"temperature": temperature}
        # Thinking burns seconds of reasoning tokens before the first output
        # token. Low-latency tiers (copilot, coach, utility calls) don't need
        # deliberation — see _gemini_thinking for the per-family settings.
        kwargs.update(_gemini_thinking(name, tier))
        if max_tokens:
            kwargs["max_output_tokens"] = max_tokens
        return ChatGoogleGenerativeAI(
            model=name, google_api_key=key, max_retries=2, **kwargs,
        )
    if p == "openai":
        from langchain_openai import ChatOpenAI
        kwargs = {}
        reasoning = name.startswith(_OPENAI_NO_TEMPERATURE)
        if temperature is not None and not reasoning:
            kwargs["temperature"] = temperature
        if reasoning and tier in _LOW_LATENCY_TIERS:
            kwargs["reasoning_effort"] = "low"
        if max_tokens:
            kwargs["max_tokens"] = max_tokens
        # stream_usage: report token counts on streamed calls too (token meter).
        return ChatOpenAI(
            model=name, api_key=key, max_retries=2, stream_usage=True, **kwargs,
        )
    if p == "chatgpt":
        # Sign in with ChatGPT: the user's plan via the public Responses API
        # with the plan's OAuth access token as the bearer. OpenAI requires
        # store=false + stream=true on every request (streaming=True makes even
        # ainvoke stream). Verified against a real Plus token (2026-10-01):
        #   * system messages are rejected (400) → _PlanChat moves them into the
        #     Responses API `instructions` field;
        #   * max_output_tokens is rejected → never sent (no output cap);
        #   * reasoning effort accepts none/low/medium/high (not "minimal");
        #     "low" gave the steadiest first-token time (~1.7-3s on gpt-5.6-luna).
        from langchain_openai import ChatOpenAI
        kwargs = {"reasoning_effort": "low"} if tier in _LOW_LATENCY_TIERS else {}
        return _PlanChat(ChatOpenAI(
            model=name, api_key=key, base_url="https://api.openai.com/v1",
            use_responses_api=True, store=False, streaming=True,
            stream_usage=True, max_retries=1, **kwargs,
        ))
    if p == "anthropic":
        from langchain_anthropic import ChatAnthropic
        kwargs = {} if temperature is None else {"temperature": temperature}
        # Sonnet/Opus 5.x think adaptively at a "high" default effort; the
        # low-latency tiers only reach them as Haiku's fallback, so dial it down.
        # Haiku 4.5 doesn't accept the effort parameter.
        if tier in _LOW_LATENCY_TIERS and "haiku" not in name:
            kwargs["effort"] = "low"
        # langchain-anthropic defaults max_tokens to 1024, which truncates
        # resumes/dossiers mid-sentence — raise it for every call.
        return ChatAnthropic(
            model=name, api_key=key, max_tokens=max_tokens or 8192,
            max_retries=2, **kwargs,
        )
    raise ValueError(f"Unknown LLM provider: {p}")


class _PlanChat:
    """ChatGPT-plan model adapter: plan requests reject system messages, so
    their text moves into the Responses API `instructions` field. Otherwise a
    pass-through, so callers keep using `.astream` / `.ainvoke` as usual."""

    def __init__(self, inner):
        self._inner = inner

    @staticmethod
    def _split(messages, kwargs):
        from langchain_core.messages import SystemMessage
        if isinstance(messages, str):
            return messages, kwargs
        system = [m for m in messages if isinstance(m, SystemMessage)]
        if not system:
            return messages, kwargs
        rest = [m for m in messages if not isinstance(m, SystemMessage)]
        instructions = "\n\n".join(content_text(m) for m in system)
        if kwargs.get("instructions"):
            instructions = f"{kwargs['instructions']}\n\n{instructions}"
        return rest, {**kwargs, "instructions": instructions}

    def astream(self, messages, **kwargs):
        messages, kwargs = self._split(messages, kwargs)
        return self._inner.astream(messages, **kwargs)

    async def ainvoke(self, messages, **kwargs):
        messages, kwargs = self._split(messages, kwargs)
        return await self._inner.ainvoke(messages, **kwargs)

    def __getattr__(self, name):  # model / model_name / etc. for logging
        return getattr(self._inner, name)


# ── Token meter ──────────────────────────────────────────────────────────────
# Every LLM call reports its token usage here, tagged with the app feature that
# made it (a request-scoped contextvar set by main.py's FeatureTagMiddleware and
# refined by routes, e.g. "chat:copilot"). Logged to sidecar.log as one line per
# call and accumulated in memory for GET /metrics/usage — the baseline every
# token-reduction change is measured against.
_feature: contextvars.ContextVar[str] = contextvars.ContextVar("llm_feature", default="other")
_usage_lock = threading.Lock()
_usage: dict[tuple[str, str], dict] = {}
_usage_since = time.time()


def set_feature(name: str) -> None:
    """Tag LLM calls made in the current request context with an app feature."""
    _feature.set(name or "other")


def current_feature() -> str:
    return _feature.get()


def record_usage(model: str, usage: dict | None, feature: str | None = None) -> None:
    """Record one call's usage (a LangChain `usage_metadata` dict). Never raises."""
    try:
        usage = usage or {}
        tin = int(usage.get("input_tokens") or 0)
        tout = int(usage.get("output_tokens") or 0)
        details = usage.get("input_token_details") or {}
        cached = int(details.get("cache_read") or 0)
        feature = feature or current_feature()
        with _usage_lock:
            row = _usage.setdefault((feature, model), {
                "feature": feature, "model": model, "calls": 0,
                "input_tokens": 0, "cached_input_tokens": 0, "output_tokens": 0,
            })
            row["calls"] += 1
            row["input_tokens"] += tin
            row["cached_input_tokens"] += cached
            row["output_tokens"] += tout
        print(f"[usage] feature={feature} model={model} in={tin} cached={cached} "
              f"out={tout}", flush=True)
    except Exception:  # noqa: BLE001 — metering must never break a call
        pass


def record_estimate(model: str, messages, output_chars: int) -> None:
    """Meter a stream the client cancelled (the copilot drops a speculative
    answer when the interviewer keeps talking). The provider bills its prompt,
    but streams report usage only at the end, so estimate ~4 chars per token —
    under "<feature>:cancelled", so estimates never mix with exact counts."""
    try:
        chars = sum(len(content_text(m)) for m in messages)
        record_usage(model, {"input_tokens": chars // 4, "output_tokens": output_chars // 4},
                     feature=f"{current_feature()}:cancelled")
    except Exception:  # noqa: BLE001 — metering must never break a call
        pass


def merge_usage(acc: dict | None, chunk) -> dict | None:
    """Fold a streamed chunk's `usage_metadata` into a running total (LangChain
    emits additive per-chunk usage, usually on the final chunk only)."""
    u = getattr(chunk, "usage_metadata", None)
    if not u:
        return acc
    if not acc:
        return dict(u)
    from langchain_core.messages.ai import add_usage
    return dict(add_usage(acc, u))


def usage_snapshot() -> dict:
    """Totals since sidecar start, per (feature, model)."""
    with _usage_lock:
        rows = [dict(r) for r in _usage.values()]
    rows.sort(key=lambda r: (r["feature"], r["model"]))
    return {"since": _usage_since, "rows": rows}


def is_auth_error(exc: Exception) -> bool:
    """True when the failure is a bad/missing API key. Every candidate model of
    the provider shares the key, so callers should stop walking the candidate
    list and surface a fix-your-key message instead of burning retries."""
    s = str(exc).lower()
    if any(m in s for m in (
        "api key not valid",         # gemini
        "api_key_invalid",           # gemini (status detail)
        "invalid api key",           # generic
        "incorrect api key",         # openai
        "invalid x-api-key",         # anthropic
        "authentication_error",      # anthropic/openai error type
        "authenticationerror",
    )):
        return True
    # PERMISSION_DENIED alone is NOT proof of a bad key: a model the account
    # can't use (e.g. Gemini 2.5, now restricted to accounts that already used
    # it) fails the same way — and THAT case must fall through to the next
    # candidate model instead of stopping with "your key was rejected".
    return "permission_denied" in s and "api key" in s


def is_quota_error(exc: Exception) -> bool:
    """ChatGPT plan usage exhausted / unavailable (e.g. Plus's shared 5-hour
    limit). Like a dead key, it fails every model of that provider, so callers
    skip straight to the fallback provider."""
    s = str(exc).lower()
    return ("subscription_sharing_usage_limit_exceeded" in s
            or "subscription_sharing_usage_unavailable" in s)


def provider_down(exc: Exception) -> bool:
    """True when no other model of the same provider can succeed either."""
    return is_auth_error(exc) or is_quota_error(exc)


def provider_down_message(cfg: LLMConfig, provider: str, exc: Exception) -> str:
    if is_quota_error(exc):
        return ("Your ChatGPT plan's usage limit is reached for now (Plus shares a "
                "5-hour limit across apps). Set a fallback in **Settings → AI routing**, "
                "or try again later.")
    return auth_error_message(cfg, provider)


def auth_error_message(cfg: LLMConfig, provider: str | None = None) -> str:
    p = provider or provider_of(cfg)
    label = PROVIDER_LABELS.get(p, p)
    return (
        f"Your {label} API key was rejected. "
        "Open **Settings → API Keys** and paste a valid key (or switch provider)."
    )


# ── Response/stream content helpers ──────────────────────────────────────────
# Anthropic (and gemini with thinking) can return content as a list of blocks
# rather than a plain string; flatten to text before JSON-encoding for SSE.

def content_text(message_or_chunk) -> str:
    c = getattr(message_or_chunk, "content", message_or_chunk)
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        out = []
        for part in c:
            if isinstance(part, str):
                out.append(part)
            elif isinstance(part, dict) and part.get("type") in (None, "text", "output_text"):
                out.append(part.get("text", ""))
        return "".join(out)
    return "" if c is None else str(c)


# ── Lenient JSON parsing (shared by application / autofill / inbox) ─────────

def _strip_fences(text: str) -> str:
    t = text.strip()
    t = re.sub(r"^```(?:json)?\s*", "", t, flags=re.MULTILINE)
    t = re.sub(r"\s*```$", "", t, flags=re.MULTILINE)
    return t.strip()


def loads_lenient(text: str) -> dict:
    """Parse the first complete JSON object out of a model response.

    Tolerates leading prose, code fences, and trailing junk after the object
    (some models append stray braces, which makes plain `json.loads` raise
    "Extra data"). Uses `raw_decode` so we take the first balanced object and
    ignore whatever follows."""
    t = _strip_fences(text)
    start = t.find("{")
    if start == -1:
        raise ValueError("no JSON object found in model output")
    obj, _ = json.JSONDecoder().raw_decode(t[start:])
    return obj


async def generate_raw(
    cfg: LLMConfig,
    prompt: str,
    *,
    tier: str = "smart",
    temperature: float | None = 0.3,
    max_tokens: int | None = None,
) -> tuple[str, str]:
    """One-shot text call, walking the selected provider's candidate models and
    then any fallback providers (see `routes_for`), so a 404/unavailable model
    or a dead key doesn't block the feature. Returns (response_text,
    model_name); raises if every route fails."""
    last_err: Exception | None = None
    dead: set[str] = set()       # providers whose key was rejected
    for p, name in routes_for(cfg, tier):
        if p in dead:
            continue
        try:
            model = make_chat_model(
                cfg, name, tier=tier, temperature=temperature, max_tokens=max_tokens,
                provider=p,
            )
            resp = await model.ainvoke([HumanMessage(content=prompt)])
        except Exception as exc:  # model unavailable — try the next route
            last_err = exc
            if provider_down(exc):
                # Same key / plan for this provider's other models — skip
                # them, but still try fallback providers.
                dead.add(p)
                last_err = RuntimeError(provider_down_message(cfg, p, exc))
            continue
        record_usage(name, getattr(resp, "usage_metadata", None))
        return content_text(resp), name
    raise last_err or RuntimeError("No model available")


async def generate_json(
    cfg: LLMConfig,
    prompt: str,
    *,
    tier: str = "smart",
    temperature: float = 0.3,
) -> tuple[dict, str]:
    """`generate_raw` + lenient object parse. Returns (parsed_dict, model_name).
    Raises ValueError/JSONDecodeError if the winning model returned non-JSON
    output (a different model wouldn't fix a prompt-shape problem)."""
    raw, name = await generate_raw(cfg, prompt, tier=tier, temperature=temperature)
    return loads_lenient(raw), name


# ── Embeddings (best-effort, for RAG) ────────────────────────────────────────

def make_embeddings(cfg: LLMConfig):
    """Return (embeddings, cache_namespace) for RAG retrieval, or (None, "").

    "local" (what Rust sends unless AI routing's per-feature Embeddings row
    names a provider) or "" runs the on-device model (local_embed.py): no API
    call, no key, works with every provider including the ChatGPT plan. An
    explicit gemini | openai uses that provider instead; either way the other
    options are fallbacks. RAG is strictly best-effort —
    callers treat (None, "") as "skip retrieval". The namespace keys the
    on-disk vector cache so vectors from different embedding models never mix.
    """
    p = (cfg.embeddings_provider or "").strip().lower()

    def _local():
        import local_embed
        return local_embed.make()

    def _gemini():
        from langchain_google_genai import GoogleGenerativeAIEmbeddings
        # text-embedding-004 was retired (404) — that silently turned RAG off.
        return (
            GoogleGenerativeAIEmbeddings(
                model="models/gemini-embedding-001", google_api_key=cfg.gemini_api_key,
            ),
            "gemini:gemini-embedding-001",
        )

    def _openai():
        from langchain_openai import OpenAIEmbeddings
        return (
            OpenAIEmbeddings(model="text-embedding-3-small", api_key=cfg.openai_api_key),
            "openai:text-embedding-3-small",
        )

    builders = []
    if p == "openai":
        builders = [_openai]
    elif p == "gemini":
        builders = [_gemini]
    # Fallbacks when the preferred builder can't run (no download yet, no key).
    builders.append(_local)
    if cfg.gemini_api_key and _gemini not in builders:
        builders.append(_gemini)
    if cfg.openai_api_key and _openai not in builders:
        builders.append(_openai)

    for build in builders:
        try:
            return build()
        except Exception:
            continue
    return None, ""
