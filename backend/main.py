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


def _preimport_llm_packages() -> None:
    """Import the LangChain partner packages in the background so the first chat
    doesn't pay their cold import (langchain_google_genai alone is ~30s on a
    cold disk cache — the sidecar imports them lazily per provider, and that
    stall used to land on the user's first message). Imports only: no model
    load, no GPU, no network — unlike the old model warmup this cannot freeze
    the app at launch."""
    for pkg in ("langchain_google_genai", "langchain_openai", "langchain_anthropic"):
        try:
            __import__(pkg)
        except Exception:
            pass  # missing optional provider — the factory reports it per-request


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


@app.get("/health")
async def health():
    return {"status": "ok"}


if __name__ == "__main__":
    port = int(os.environ.get("INTERPREP_PORT", "8765"))
    uvicorn.run(app, host="127.0.0.1", port=port, log_level="error")
