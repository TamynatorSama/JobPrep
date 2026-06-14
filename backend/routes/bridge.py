"""Bridge auth + config endpoints for the browser extension.

  POST /config/seed   {llm: {...}}     -> refresh the in-memory LLM config
  GET  /config/ping                    -> connectivity + auth check for the popup

Both require the shared secret in the ``X-InterPrep-Token`` header. The token is
established only from the env var set by the Rust shell (see runtime_config); it
can never be set over HTTP, so a malicious page can't authenticate itself.
"""
from __future__ import annotations

from fastapi import APIRouter, Depends, Header, HTTPException

import runtime_config
from models import SeedConfigRequest

router = APIRouter()


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
    return {"ok": True}


@router.get("/ping")
async def ping(_: None = Depends(require_token)):
    import llm_provider as llm_factory
    cfg = runtime_config.get_llm_config()
    return {"ok": True, "has_key": llm_factory.missing_key_error(cfg) is None}
