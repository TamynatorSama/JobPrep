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

import json
import re

from langchain_core.messages import HumanMessage

from models import LLMConfig

PROVIDER_LABELS = {
    "gemini": "Gemini",
    "openai": "OpenAI",
    "anthropic": "Anthropic (Claude)",
}

# Candidate models per provider+tier, strongest-first. Routes walk the list so
# a model that 404s on the user's account falls through to a stable one.
# "fast"  = chat/coach/utility calls + the resume orchestrator's own router call;
# "mid"   = cost/quality middle ground the resume orchestrator routes easy/medium
#           jobs to (see model_router.py);
# "smart" = persona consistency + hard structured resume work.
_MODELS: dict[str, dict[str, list[str]]] = {
    "gemini": {
        "fast":  ["gemini-2.5-flash"],
        "mid":   ["gemini-2.5-pro", "gemini-2.5-flash"],
        "smart": ["gemini-3.1-pro-preview", "gemini-3-pro-preview", "gemini-2.5-pro"],
    },
    "openai": {
        "fast":  ["gpt-4o-mini"],
        "mid":   ["gpt-4.1", "gpt-4o"],
        "smart": ["gpt-5", "gpt-4.1", "gpt-4o"],
    },
    "anthropic": {
        "fast":  ["claude-haiku-4-5"],
        "mid":   ["claude-sonnet-4-6"],
        "smart": ["claude-opus-4-8", "claude-sonnet-4-6"],
    },
}

# OpenAI reasoning-family models reject explicit temperature (must use the
# model default) — skip the param rather than burn a failed call.
_OPENAI_NO_TEMPERATURE = ("gpt-5", "o1", "o3", "o4")


def provider_of(cfg: LLMConfig) -> str:
    return (cfg.provider or "gemini").strip().lower()


def api_key_for(cfg: LLMConfig) -> str:
    """The selected provider's API key."""
    return {
        "gemini": cfg.gemini_api_key,
        "openai": cfg.openai_api_key,
        "anthropic": cfg.anthropic_api_key,
    }.get(provider_of(cfg), "")


def missing_key_error(cfg: LLMConfig) -> str | None:
    """User-facing error when the selected provider isn't usable, else None."""
    p = provider_of(cfg)
    if p not in PROVIDER_LABELS:
        return f"Unknown AI provider `{p}`. Open **Settings → API Keys**."
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
    p = provider_of(cfg)
    tiers = _MODELS.get(p) or _MODELS["gemini"]
    return tiers.get(tier) or tiers["fast"]


def make_chat_model(
    cfg: LLMConfig,
    model_name: str | None = None,
    *,
    tier: str = "fast",
    temperature: float | None = None,
):
    """Construct a LangChain chat model for the selected provider.

    `model_name=None` uses the first candidate of `tier`. All returned models
    support `.ainvoke` and `.astream` with langchain-core message types.
    """
    p = provider_of(cfg)
    name = model_name or candidate_models(cfg, tier)[0]

    # LangChain clients default to ~6 retries with exponential backoff, which
    # turns a dead key / down model into a minute-long stall before our own
    # candidate-fallback can act. One retry is enough for a transient blip;
    # anything persistent should fail fast so the next candidate gets a shot.
    if p == "gemini":
        from langchain_google_genai import ChatGoogleGenerativeAI
        kwargs = {} if temperature is None else {"temperature": temperature}
        # Gemini 2.5 models ship with DYNAMIC "thinking" on by default, which
        # burns seconds of reasoning tokens before the first output token. The
        # fast tier exists for latency (copilot live answers, coach chat,
        # router/summary utility calls) — none of it needs deliberation, so
        # zero the budget there. Smart/mid tiers keep the default (persona
        # consistency + structured resume work benefit from it).
        if tier == "fast" and "2.5" in name:
            kwargs["thinking_budget"] = 0
        return ChatGoogleGenerativeAI(
            model=name, google_api_key=cfg.gemini_api_key, max_retries=2, **kwargs,
        )
    if p == "openai":
        from langchain_openai import ChatOpenAI
        kwargs = {}
        if temperature is not None and not name.startswith(_OPENAI_NO_TEMPERATURE):
            kwargs["temperature"] = temperature
        return ChatOpenAI(
            model=name, api_key=cfg.openai_api_key, max_retries=2, **kwargs,
        )
    if p == "anthropic":
        from langchain_anthropic import ChatAnthropic
        kwargs = {} if temperature is None else {"temperature": temperature}
        # langchain-anthropic defaults max_tokens to 1024, which truncates
        # resumes/dossiers mid-sentence — raise it for every call.
        return ChatAnthropic(
            model=name, api_key=cfg.anthropic_api_key, max_tokens=8192,
            max_retries=2, **kwargs,
        )
    raise ValueError(f"Unknown LLM provider: {p}")


def is_auth_error(exc: Exception) -> bool:
    """True when the failure is a bad/missing API key. Every candidate model of
    the provider shares the key, so callers should stop walking the candidate
    list and surface a fix-your-key message instead of burning retries."""
    s = str(exc).lower()
    return any(m in s for m in (
        "api key not valid",         # gemini
        "api_key_invalid",           # gemini (status detail)
        "invalid api key",           # generic
        "incorrect api key",         # openai
        "invalid x-api-key",         # anthropic
        "authentication_error",      # anthropic/openai error type
        "authenticationerror",
        "permission_denied",
    ))


def auth_error_message(cfg: LLMConfig) -> str:
    label = PROVIDER_LABELS.get(provider_of(cfg), provider_of(cfg))
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
            elif isinstance(part, dict) and part.get("type") in (None, "text"):
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
    temperature: float = 0.3,
) -> tuple[str, str]:
    """One-shot text call against the selected provider, walking the candidate
    models so a 404/unavailable model doesn't block the feature. Returns
    (response_text, model_name); raises the last error if every model fails."""
    last_err: Exception | None = None
    for name in candidate_models(cfg, tier):
        try:
            model = make_chat_model(cfg, name, tier=tier, temperature=temperature)
            resp = await model.ainvoke([HumanMessage(content=prompt)])
        except Exception as exc:  # model unavailable — try the next candidate
            last_err = exc
            if is_auth_error(exc):
                # Same key for every candidate — no point walking the rest.
                raise RuntimeError(auth_error_message(cfg)) from exc
            continue
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

    Prefers the selected provider when it has an embeddings API; anthropic
    doesn't, so it falls back to any other configured provider. RAG is strictly
    best-effort — callers treat (None, "") as "skip retrieval". The namespace
    keys the on-disk vector cache so vectors from different embedding models
    never mix.
    """
    p = provider_of(cfg)

    def _gemini():
        from langchain_google_genai import GoogleGenerativeAIEmbeddings
        return (
            GoogleGenerativeAIEmbeddings(
                model="models/text-embedding-004", google_api_key=cfg.gemini_api_key,
            ),
            "gemini:text-embedding-004",
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
    # Fallbacks for providers without embeddings (anthropic) or when the
    # preferred builder can't run.
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
