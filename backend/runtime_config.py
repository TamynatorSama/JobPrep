"""Process-wide runtime config for the browser-extension bridge.

Holds two secrets in memory only (never written to disk):

  * ``llm``      — the LLM config (provider toggle + per-provider API keys), so
                   extension requests don't have to carry keys. The Gemini key
                   is seeded from ``INTERPREP_GEMINI_KEY`` at spawn (covers the
                   window before the shell's first seed call); the full config
                   arrives over ``POST /config/seed`` on startup and whenever
                   the user saves Settings (so changes don't need a restart).
  * ``token``    — a shared secret minted by the Rust shell and passed in via
                   ``INTERPREP_BRIDGE_TOKEN``. Every sensitive bridge endpoint
                   (``/config``, ``/store``, ``/autofill``) requires it in the
                   ``X-InterPrep-Token`` header. This is the gate that stops any
                   random web page the user visits from reading their resume or
                   spending their LLM quota against this localhost server.

The token is fixed for the lifetime of the process and is NOT settable over
HTTP — otherwise a malicious page could overwrite it with its own value and
then authenticate. Only the env var (set by the trusted parent process) and
this module's startup read can establish it.
"""
from __future__ import annotations

import os
import threading

from models import LLMConfig

_lock = threading.Lock()
_state = {
    "llm": {
        "provider": os.environ.get("INTERPREP_LLM_PROVIDER", "gemini"),
        "gemini_api_key": os.environ.get("INTERPREP_GEMINI_KEY", ""),
    },
    "token": os.environ.get("INTERPREP_BRIDGE_TOKEN", ""),
}


def get_llm_config() -> LLMConfig:
    with _lock:
        return LLMConfig(**_state["llm"])


def set_llm_config(cfg: LLMConfig) -> None:
    with _lock:
        _state["llm"] = cfg.model_dump()


def get_token() -> str:
    with _lock:
        return _state["token"]
