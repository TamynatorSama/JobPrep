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


@asynccontextmanager
async def lifespan(_app: FastAPI):
    # Warm the Playwright browser in the background so /health responds immediately.
    # The Rust sidecar polls /health with a 30-second timeout; blocking here would
    # cause that timeout to fire before the app is reachable.
    async def _warm():
        try:
            from agents.company_research.browser_manager import get_browser
            await get_browser()
        except Exception:
            pass

    asyncio.create_task(_warm())
    yield

    try:
        from agents.company_research.browser_manager import shutdown
        await shutdown()
    except Exception:
        pass


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


@app.get("/health")
async def health():
    return {"status": "ok"}


if __name__ == "__main__":
    port = int(os.environ.get("INTERPREP_PORT", "8765"))
    uvicorn.run(app, host="127.0.0.1", port=port, log_level="error")
