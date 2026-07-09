"""Company-research SSE endpoint.

Backed by the embedded `research_scraper` engine (LangGraph plan → search → scrape →
reflect → peer → audit → synthesize). No browser/logins: it escalates through hidden
JSON endpoints and TLS impersonation, pulls from EDGAR, GitHub, Levels.fyi, Glassdoor,
Comparably, RepVue and Google (Gemini Search grounding), audits citations, and fills
thin comp/interview sections from comparable companies.

The engine's LLM router speaks the OpenAI-compatible wire format and builds a
fallback chain (custom lane → gemini → …). We map the user's selected provider
(Settings toggle) onto the custom lane so it is tried FIRST, and pass the spare
gemini key through as a fallback lane. The Gemini key additionally powers the
Google Search grounding source regardless of which provider runs the chain.

The SSE event contract is unchanged: {type: stage|token|error|done}."""
import json
import sys
import traceback

from fastapi import APIRouter
from sse_starlette.sse import EventSourceResponse

import llm_provider as llm_factory
from models import CompanyResearchRequest, LLMConfig
from research_scraper import research_events
# research_scraper installs its modules top-level (editable install); `runtime`
# is its per-request override contextvar — the same module the engine reads,
# so wrapping the stream in `overrides(...)` configures the LLM router lanes.
from runtime import overrides as engine_overrides

router = APIRouter()

# Selected provider → the OpenAI-compatible "custom" lane of the engine's
# router (base_url, key getter, default model). Anthropic rides its official
# OpenAI-compatibility endpoint because the engine only speaks that protocol.
_CUSTOM_LANES = {
    "gemini": (
        "https://generativelanguage.googleapis.com/v1beta/openai",
        lambda c: c.gemini_api_key,
        "gemini-2.5-flash",
    ),
    "openai": (
        "https://api.openai.com/v1",
        lambda c: c.openai_api_key,
        "gpt-4o-mini",
    ),
    "anthropic": (
        "https://api.anthropic.com/v1",
        lambda c: c.anthropic_api_key,
        "claude-haiku-4-5",
    ),
}


def _engine_overrides(cfg: LLMConfig) -> dict:
    """Map the app's LLMConfig onto research_scraper's override keys."""
    kw: dict = {}
    p = llm_factory.provider_of(cfg)

    if p in _CUSTOM_LANES:
        base_url, key_of, default_model = _CUSTOM_LANES[p]
        key = key_of(cfg)
        if key:
            kw["llm_base_url"] = base_url
            kw["llm_api_key"] = key
            kw["llm_model"] = cfg.model.strip() or default_model

    # The spare gemini key becomes a fallback lane and also powers the
    # Google Search grounding source.
    if cfg.gemini_api_key:
        kw["gemini_api_key"] = cfg.gemini_api_key
    return kw


@router.post("/stream")
async def company_research_stream(req: CompanyResearchRequest):
    async def generate():
        cfg = req.llm
        print(
            f"[company-research] /stream company={req.company!r} role={req.role!r} "
            f"provider={llm_factory.provider_of(cfg)!r}",
            file=sys.stderr, flush=True,
        )
        yield {"data": json.dumps({
            "type": "stage",
            "content": "\n📡 **Request received** — starting research…\n",
        })}

        key_err = llm_factory.missing_key_error(cfg)
        if key_err:
            yield {"data": json.dumps({"type": "error", "content": key_err})}
            return
        if not req.company.strip():
            yield {"data": json.dumps({"type": "error", "content": "Company name is required."})}
            return

        try:
            with engine_overrides(**_engine_overrides(cfg)):
                async for ev in research_events(
                    req.company,
                    req.role or "",
                    location=req.location or "",
                    job_description=req.job_description or "",
                    tailored_resume=req.tailored_resume or "",
                ):
                    etype = ev.get("type")
                    if etype == "stage":
                        yield {"data": json.dumps({
                            "type": "stage", "content": f"\n\n---\n{ev['content']}\n\n",
                        })}
                    elif etype == "token":
                        yield {"data": json.dumps({"type": "token", "content": ev["content"]})}
                    elif etype == "error":
                        content = str(ev["content"])
                        # A rejected key fails every engine lane the same way —
                        # surface the fix instead of the raw provider JSON.
                        if llm_factory.is_auth_error(Exception(content)) \
                                or "valid api key" in content.lower():
                            content = llm_factory.auth_error_message(cfg)
                        yield {"data": json.dumps({"type": "error", "content": content})}
                    elif etype == "done":
                        yield {"data": json.dumps({"type": "done"})}
                    # "result" (structured fields) is not part of the frontend contract; skip.
        except Exception as exc:
            tb = traceback.format_exc()
            print(f"[company-research] FATAL: {exc}\n{tb}", file=sys.stderr, flush=True)
            yield {"data": json.dumps({
                "type": "error", "content": f"{type(exc).__name__}: {exc}",
            })}

    return EventSourceResponse(generate())
