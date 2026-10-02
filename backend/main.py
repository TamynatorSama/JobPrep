import os
import threading
from contextlib import asynccontextmanager

import uvicorn
from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware

from routes.chat import router as chat_router
from routes.research import router as research_router
from routes.company_research import router as company_research_router
from routes.application import router as application_router
from routes.cheatsheet import router as cheatsheet_router
from routes.voice import router as voice_router
from routes.bridge import router as bridge_router
from routes.store import router as store_router
from routes.autofill import router as autofill_router
from routes.inbox import router as inbox_router
from routes.metrics import router as metrics_router
from routes.model_catalog import router as models_router

import llm_provider as llm_factory

# Path prefix → app feature, for the token meter (llm_provider.record_usage).
# Longest prefix first. Routes may refine it (chat sets "chat:<mode>").
_FEATURES = (
    ("/application/knockout-screen", "recruiter-screen"),
    ("/application", "resume-tailor"),
    ("/company-research", "company-research"),
    ("/research", "role-fit-research"),
    ("/cheatsheet", "cheatsheet"),
    ("/autofill", "extension:autofill"),
    ("/inbox", "extension:jd-extract"),
    ("/config", "warmup"),
    ("/chat", "chat"),
)


class FeatureTagMiddleware:
    """Tag every LLM call with the feature whose request made it.

    Pure ASGI (not BaseHTTPMiddleware) on purpose: the contextvar is set in the
    request's own task before the app runs, and the SSE responses' generator
    tasks are spawned from inside it, so they inherit the tag."""

    def __init__(self, app):
        self.app = app

    async def __call__(self, scope, receive, send):
        if scope["type"] == "http":
            path = scope.get("path", "")
            llm_factory.set_feature(
                next((name for prefix, name in _FEATURES if path.startswith(prefix)), "other")
            )
        await self.app(scope, receive, send)


def _preimport_llm_packages() -> None:
    """Import the LangChain partner packages in the background so the first chat
    doesn't pay their cold import (langchain_google_genai alone is ~30s on a
    cold disk cache — the sidecar imports them lazily per provider, and that
    stall used to land on the user's first message). No GPU, no network, and
    the only model load is the small CPU embedder — unlike the old model warmup
    this cannot freeze the app at launch."""
    for pkg in ("langchain_google_genai", "langchain_openai", "langchain_anthropic"):
        try:
            __import__(pkg)
        except Exception:
            pass  # missing optional provider — the factory reports it per-request
    # Then the heavy modules the routes import lazily (kept off the boot path so
    # /health answers fast): langgraph for role-fit research, the company-research
    # engine, and python-docx for resume rendering.
    for mod in ("agents.workflow", "research_scraper", "resume_docx", "routes.docx_editor"):
        try:
            __import__(mod)
        except Exception:
            pass  # surfaces as a clear error on the request that needs it
    # The on-device RAG embedder (34 MB, CPU), if it's already downloaded — the
    # first download happens on first use instead, never at boot.
    try:
        import local_embed
        local_embed.warm_if_cached()
    except Exception:
        pass


@asynccontextmanager
async def lifespan(_app: FastAPI):
    # No MODEL is warmed at startup. Company research is the in-process
    # `research_scraper` engine (HTTP/JSON, no browser), and the interview voice
    # stack (faster-whisper STT + the chosen TTS engine) is warmed ON DEMAND via
    # POST /voice/prepare when the user starts a mock interview, behind the
    # "Preparing engine…" modal. Warming models at boot stacked the STT load and
    # the VibeVoice cold synth on the same device and froze the app on launch.
    # Pure Python imports are safe to pre-warm, and big enough to matter.
    threading.Thread(target=_preimport_llm_packages, daemon=True).start()
    yield


app = FastAPI(title="InterPrep Backend", version="0.2.0", lifespan=lifespan)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_methods=["*"],
    allow_headers=["*"],
)

app.include_router(chat_router, prefix="/chat")
app.include_router(research_router, prefix="/research")
app.include_router(company_research_router, prefix="/company-research")
app.include_router(application_router, prefix="/application")
app.include_router(cheatsheet_router, prefix="/cheatsheet")
app.include_router(voice_router, prefix="/voice")
# Browser-extension bridge. /config, /store and /autofill are all guarded by the
# X-InterPrep-Token shared secret (set via INTERPREP_BRIDGE_TOKEN by the shell).
app.include_router(bridge_router, prefix="/config")
app.include_router(store_router, prefix="/store")
app.include_router(autofill_router, prefix="/autofill")
app.include_router(inbox_router, prefix="/inbox")
app.include_router(metrics_router, prefix="/metrics")
app.include_router(models_router, prefix="/models")
app.add_middleware(FeatureTagMiddleware)


@app.get("/health")
async def health():
    return {"status": "ok"}


if __name__ == "__main__":
    port = int(os.environ.get("INTERPREP_PORT", "8765"))
    uvicorn.run(app, host="127.0.0.1", port=port, log_level="error")
