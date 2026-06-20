import asyncio
import os
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


@asynccontextmanager
async def lifespan(_app: FastAPI):
    # Company research is now the in-process `research_scraper` engine (HTTP/JSON,
    # no browser), so there's nothing to pre-warm for it.
    async def _warm_voice():
        # Load the TTS model + reference clip off the event loop so the first
        # interview question doesn't pay the cold start. Heavy (torch + model
        # download on first ever run) so it runs in a thread, best-effort.
        try:
            from routes.voice import prewarm
            await asyncio.to_thread(prewarm)
        except Exception:
            pass

    asyncio.create_task(_warm_voice())
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
