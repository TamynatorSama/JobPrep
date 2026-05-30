"""Voice services for the live mock interview.

  POST /voice/tts    {text}        -> {audio_b64, sample_rate}   (VoxCPM-0.5B)
  POST /voice/stt    {audio_b64}   -> {text}                     (faster-whisper)
  GET  /voice/status               -> capability + device report

Both models are heavy (torch + a model download) and optional: the rest of
the app works without them. So everything here is lazy-loaded and degrades to
a clear error instead of crashing the sidecar at import time. Device is chosen
automatically — CUDA when available, else CPU — and surfaced via /voice/status
so the UI can tell the user which one is in use.
"""
from __future__ import annotations

import asyncio
import base64
import io
import threading
import wave

from fastapi import APIRouter
from pydantic import BaseModel

router = APIRouter()


# ── request models ───────────────────────────────────────────────────────────
class TtsRequest(BaseModel):
    text: str


class SttRequest(BaseModel):
    # Base64-encoded WAV (any sample rate / channels; we normalize on read).
    audio_b64: str


# ── lazy singletons ──────────────────────────────────────────────────────────
_lock = threading.Lock()
_state = {
    "device": None,        # "cuda" | "cpu"
    "tts": None,           # VoxCPM model
    "tts_sr": 16000,       # VoxCPM output sample rate
    "stt": None,           # faster-whisper WhisperModel
    "import_error": None,  # str if torch/models can't be imported
    "ref_wav": None,       # path to the fixed reference voice (consistency)
    "ref_text": None,      # transcript of the reference clip
}

# The interviewer's voice. VoxCPM picks a fresh random timbre each call unless
# given a reference clip to clone — so we synthesize this line ONCE, cache it,
# and clone it for every subsequent line so the voice stays consistent across
# the whole interview (and across sessions, since the clip is persisted).
REF_TEXT = "Let's get started with your interview. Tell me a bit about yourself."


def _device() -> str:
    if _state["device"]:
        return _state["device"]
    try:
        import torch
        _state["device"] = "cuda" if torch.cuda.is_available() else "cpu"
    except Exception as exc:
        _state["import_error"] = f"torch unavailable: {exc}"
        _state["device"] = "cpu"
    return _state["device"]


def _get_tts():
    """Load VoxCPM-0.5B once. Raises with a helpful message if deps missing."""
    if _state["tts"] is not None:
        return _state["tts"]
    with _lock:
        if _state["tts"] is None:
            from voxcpm import VoxCPM  # heavy; imported lazily on first use
            # load_denoiser=False: the zipenhancer denoiser only cleans prompt/
            # reference audio (we use neither), pulls heavy modelscope native
            # deps, and segfaults on this stack. Skip it — faster + stable.
            model = VoxCPM.from_pretrained(
                "openbmb/VoxCPM-0.5B", device=_device(), load_denoiser=False,
            )
            _state["tts"] = model
            # The real output sample rate lives on the wrapped inner model.
            inner = getattr(model, "tts_model", model)
            _state["tts_sr"] = int(getattr(inner, "sample_rate", _state["tts_sr"]))
    return _state["tts"]


def _get_stt():
    """Load faster-whisper once, on the auto-detected device."""
    if _state["stt"] is not None:
        return _state["stt"]
    with _lock:
        if _state["stt"] is None:
            from faster_whisper import WhisperModel
            device = _device()
            compute = "float16" if device == "cuda" else "int8"
            # "base" balances accuracy vs. latency for interview answers; bump
            # to "small"/"medium" if transcripts are weak and the box can take it.
            _state["stt"] = WhisperModel("base", device=device, compute_type=compute)
    return _state["stt"]


# ── helpers ──────────────────────────────────────────────────────────────────
def _float_to_wav_bytes(samples, sample_rate: int) -> bytes:
    """Encode a mono float array (-1..1) as 16-bit PCM WAV bytes."""
    import numpy as np

    arr = np.asarray(samples, dtype=np.float32).flatten()
    arr = np.clip(arr, -1.0, 1.0)
    pcm = (arr * 32767.0).astype("<i2").tobytes()
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(pcm)
    return buf.getvalue()


def _voice_dir():
    import os
    from pathlib import Path
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or "."
    d = Path(base) / "InterPrep" / "voice"
    d.mkdir(parents=True, exist_ok=True)
    return d


def _ensure_reference(model) -> None:
    """Make sure a fixed reference voice clip exists (generate + persist once)."""
    if _state["ref_wav"]:
        return
    ref_path = _voice_dir() / "voice_ref.wav"
    if ref_path.exists():
        _state["ref_wav"] = str(ref_path)
        _state["ref_text"] = REF_TEXT
        return
    # First ever run: synthesize the seed line (random timbre), then freeze it.
    wav = model.generate(text=REF_TEXT)
    ref_path.write_bytes(_float_to_wav_bytes(wav, int(_state["tts_sr"])))
    _state["ref_wav"] = str(ref_path)
    _state["ref_text"] = REF_TEXT


def _synthesize(text: str) -> tuple[bytes, int]:
    model = _get_tts()
    sr = int(_state["tts_sr"])
    _ensure_reference(model)
    # Clone the locked reference voice so every line sounds like one interviewer.
    wav = model.generate(
        text=text,
        prompt_wav_path=_state["ref_wav"],
        prompt_text=_state["ref_text"],
    )
    return _float_to_wav_bytes(wav, sr), sr


def _transcribe(wav_bytes: bytes) -> str:
    model = _get_stt()
    segments, _info = model.transcribe(io.BytesIO(wav_bytes), beam_size=1)
    return "".join(seg.text for seg in segments).strip()


# ── endpoints ────────────────────────────────────────────────────────────────
@router.get("/status")
async def status():
    """Report whether voice is usable + which device it runs on. The UI shows
    this so the user knows if they're on GPU (live) or CPU (laggy)."""
    device = _device()
    available = True
    detail = _state["import_error"]
    try:
        import importlib.util
        for mod in ("torch", "voxcpm", "faster_whisper"):
            if importlib.util.find_spec(mod) is None:
                available = False
                detail = f"missing dependency: {mod} (run backend/setup.ps1)"
                break
    except Exception as exc:
        available = False
        detail = str(exc)
    return {
        "available": available,
        "device": device,
        "tts_loaded": _state["tts"] is not None,
        "stt_loaded": _state["stt"] is not None,
        "detail": detail,
    }


@router.post("/tts")
async def tts(req: TtsRequest):
    text = (req.text or "").strip()
    if not text:
        return {"error": "empty text"}
    try:
        wav_bytes, sr = await asyncio.to_thread(_synthesize, text)
        return {"audio_b64": base64.b64encode(wav_bytes).decode("ascii"), "sample_rate": sr}
    except Exception as exc:
        return {"error": f"TTS failed: {exc}"}


@router.post("/stt")
async def stt(req: SttRequest):
    try:
        wav_bytes = base64.b64decode(req.audio_b64)
    except Exception as exc:
        return {"error": f"bad audio_b64: {exc}"}
    try:
        text = await asyncio.to_thread(_transcribe, wav_bytes)
        return {"text": text}
    except Exception as exc:
        return {"error": f"STT failed: {exc}"}
