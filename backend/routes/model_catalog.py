"""Model catalog + speed test for the Settings → AI routing card.

  GET  /models/registry -> {tiers: {provider: {tier: [model, ...]}},
                            feature_tiers: {feature: tier}}
  POST /models/list {llm, provider} -> {provider, models: [...]}
  POST /models/test {llm, provider, model, feature} -> {ok, model, ttft_ms,
                                                        total_ms, error}

`list` asks the provider's own list-models API with the user's key (so the
picker only offers models the account can actually call) and caches the result
for a day. `test` sends one tiny prompt through the same factory every feature
uses — same tier, thinking settings and output cap — and reports time to first
token, so the copilot model can be picked on measured speed.
"""
from __future__ import annotations

import asyncio
import hashlib
import time

from fastapi import APIRouter
from langchain_core.messages import HumanMessage
from pydantic import BaseModel

import llm_provider as llm_factory
from models import LLMConfig

router = APIRouter()

# Which registry tier each app feature runs on (mirrors the routes).
FEATURE_TIERS = {
    "copilot": "instant",
    "coach": "fast",
    "interview": "fast",
    "role_fit": "fast",
    "company_research": "fast",
    "resume_tailor": "mid",
    "cheatsheet": "smart",
    "extension": "smart",
}

_LIST_TTL_S = 24 * 3600
_list_cache: dict[tuple[str, str], tuple[float, list[str]]] = {}


class ListRequest(BaseModel):
    llm: LLMConfig = LLMConfig()
    provider: str


class TestRequest(BaseModel):
    llm: LLMConfig = LLMConfig()
    provider: str
    model: str = ""
    feature: str = "coach"


@router.get("/registry")
async def registry():
    return {"tiers": llm_factory._MODELS, "feature_tiers": FEATURE_TIERS}


def _chat_model_ids(provider: str, key: str) -> list[str]:
    """Chat-capable model ids the key can see, via the provider SDKs that the
    LangChain partner packages already pull in."""
    if provider == "gemini":
        from google import genai
        out = []
        # Hold the client: the pager fetches later pages through it, and a
        # temporary Client() is closed as soon as it's garbage-collected.
        client = genai.Client(api_key=key)
        for m in client.models.list():
            name = (m.name or "").removeprefix("models/")
            actions = getattr(m, "supported_actions", None) or []
            if not name.startswith("gemini") or (actions and "generateContent" not in actions):
                continue
            if any(x in name for x in ("embedding", "tts", "image", "live", "audio", "transcribe")):
                continue
            out.append(name)
        return out
    if provider == "openai":
        from openai import OpenAI
        ids = [m.id for m in OpenAI(api_key=key).models.list()]
        skip = ("embedding", "tts", "whisper", "dall-e", "image", "audio", "realtime",
                "transcribe", "moderation", "search", "davinci", "babbage")
        return [i for i in ids if i.startswith(("gpt-", "o1", "o3", "o4", "chatgpt"))
                and not any(x in i for x in skip)]
    if provider == "anthropic":
        from anthropic import Anthropic
        return [m.id for m in Anthropic(api_key=key).models.list(limit=100)]
    if provider == "chatgpt":
        # `key` is the plan's OAuth access token; the plan decides the models.
        return llm_factory.chatgpt_plan_models(key)
    raise ValueError(f"unknown provider {provider!r}")


def _ordered(provider: str, ids: list[str]) -> list[str]:
    """Registry models first (in tier order), then the rest newest-looking first."""
    preferred: list[str] = []
    for tier in ("instant", "fast", "mid", "smart"):
        for m in llm_factory._MODELS.get(provider, {}).get(tier, []):
            if m in ids and m not in preferred:
                preferred.append(m)
    rest = sorted((i for i in set(ids) if i not in preferred), reverse=True)
    return preferred + rest


@router.post("/list")
async def list_models(req: ListRequest):
    p = req.provider.strip().lower()
    key = llm_factory.api_key_for(req.llm, p)
    if not key:
        return {"provider": p, "models": [], "error": "Not signed in with ChatGPT" if p == "chatgpt" else f"No {p} key set"}
    cache_key = (p, hashlib.sha256(key.encode()).hexdigest()[:16])
    hit = _list_cache.get(cache_key)
    if hit and time.time() - hit[0] < _LIST_TTL_S:
        return {"provider": p, "models": hit[1], "cached": True}
    try:
        ids = await asyncio.to_thread(_chat_model_ids, p, key)
    except Exception as exc:  # noqa: BLE001 — surface, don't crash Settings
        msg = llm_factory.auth_error_message(req.llm, p) if llm_factory.is_auth_error(exc) else str(exc)
        return {"provider": p, "models": [], "error": msg[:300]}
    models = _ordered(p, ids)
    _list_cache[cache_key] = (time.time(), models)
    return {"provider": p, "models": models}


@router.post("/test")
async def test_model(req: TestRequest):
    """One tiny streamed prompt through the feature's real settings."""
    p = req.provider.strip().lower()
    tier = FEATURE_TIERS.get(req.feature, "fast")
    llm_factory.set_feature(f"settings-test:{req.feature}")
    cfg = req.llm.model_copy(update={"provider": p, "model": req.model.strip(), "fallbacks": []})
    if llm_factory.missing_key_error(cfg):
        return {"ok": False, "error": llm_factory.missing_key_error(cfg)}
    name = llm_factory.candidate_models(cfg, tier)[0]
    from routes.chat import COPILOT_MAX_TOKENS
    t0 = time.monotonic()
    ttft = None
    usage = None
    try:
        model = llm_factory.make_chat_model(
            cfg, name, tier=tier,
            max_tokens=COPILOT_MAX_TOKENS if req.feature == "copilot" else None,
        )
        async for chunk in model.astream([HumanMessage(content="Reply with the single word: ok")]):
            usage = llm_factory.merge_usage(usage, chunk)
            if ttft is None and llm_factory.content_text(chunk):
                ttft = time.monotonic() - t0
    except Exception as exc:  # noqa: BLE001
        msg = llm_factory.auth_error_message(cfg, p) if llm_factory.is_auth_error(exc) else str(exc)
        return {"ok": False, "model": name, "error": msg[:300]}
    llm_factory.record_usage(name, usage)
    return {
        "ok": ttft is not None, "model": name,
        "ttft_ms": None if ttft is None else round(ttft * 1000),
        "total_ms": round((time.monotonic() - t0) * 1000),
    }
