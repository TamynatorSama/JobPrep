"""Process-wide runtime config for the browser-extension bridge.

Holds two secrets in memory only (never written to disk):

  * ``api_key``  — the Gemini key, so extension requests don't have to carry it.
                   Seeded from the ``INTERPREP_GEMINI_KEY`` env var at spawn and
                   refreshed over ``POST /config/seed`` when the user edits it in
                   Settings (so a key change doesn't need an app restart).
  * ``token``    — a shared secret minted by the Rust shell and passed in via
                   ``INTERPREP_BRIDGE_TOKEN``. Every sensitive bridge endpoint
                   (``/config``, ``/store``, ``/autofill``) requires it in the
                   ``X-InterPrep-Token`` header. This is the gate that stops any
                   random web page the user visits from reading their resume or
                   spending their Gemini quota against this localhost server.

The token is fixed for the lifetime of the process and is NOT settable over
HTTP — otherwise a malicious page could overwrite it with its own value and
then authenticate. Only the env var (set by the trusted parent process) and
this module's startup read can establish it.
"""
from __future__ import annotations

import os
import threading

_lock = threading.Lock()
_state = {
    "api_key": os.environ.get("INTERPREP_GEMINI_KEY", ""),
    "token": os.environ.get("INTERPREP_BRIDGE_TOKEN", ""),
}


def get_api_key() -> str:
    with _lock:
        return _state["api_key"]


def set_api_key(value: str) -> None:
    with _lock:
        _state["api_key"] = value or ""


def get_token() -> str:
    with _lock:
        return _state["token"]
