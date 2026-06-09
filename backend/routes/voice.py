"""Voice services for the live mock interview.

  POST /voice/tts    {text,engine,speaker} -> {audio_b64, sample_rate}   (piper | vibe-rt)
  POST /voice/stt    {audio_b64}           -> {text}                     (faster-whisper)
  GET  /voice/status                       -> capability + device report

Two TTS engines:
  * "piper" (default) — fast neural TTS via onnxruntime. Runs faster than
    real-time on CPU, no GPU needed, ~60MB voice model. The interview voice
    unless the user opts into "vibe-rt".
  * "vibe-rt" — Microsoft VibeVoice-Realtime-0.5B, far more humanlike. Ships
    several fixed preset voices, so a *panel* of distinct interviewers is
    possible by picking a different `speaker` per turn (see VIBE_VOICES).
    torch-based; wants a GPU to keep up. Opt-in via the Settings toggle.

Everything is lazy-loaded and degrades to a clear error instead of crashing the
sidecar at import time. Device is chosen automatically — CUDA when available,
else CPU — and surfaced via /voice/status so the UI can tell the user what's in
use. Only the chosen engine is loaded; "vibe-rt" never loads unless toggled on.
"""
from __future__ import annotations

import asyncio
import base64
import io
import threading
import wave

from fastapi import APIRouter
from fastapi.responses import JSONResponse, Response, StreamingResponse
from pydantic import BaseModel

router = APIRouter()

# Default engine when a request doesn't specify one. "piper" is fast; "vibe-rt"
# is the humanlike VibeVoice voice the user opts into.
DEFAULT_ENGINE = "piper"

# Piper voice to use. The .onnx + matching .onnx.json are fetched from the
# rhasspy/piper-voices HF repo on first use (or pre-fetched by setup.ps1).
PIPER_VOICE = "en_US-amy-medium"
PIPER_BASE_URL = (
    "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/amy/medium"
)

# ── VibeVoice (realtime 0.5B) ────────────────────────────────────────────────
# Weights live on HF; the speaker *presets* (prefilled outputs) ship only in the
# GitHub repo under demo/voices/streaming_model/. The realtime model dropped the
# acoustic tokenizer, so new voices can't be encoded locally — we're limited to
# these shipped presets. That's fine: a panel needs distinct, consistent voices,
# not custom ones, and 6 English presets covers up to a 4-person panel.
VIBE_MODEL_ID = "microsoft/VibeVoice-Realtime-0.5B"
VIBE_SR = 24000           # VibeVoice output sample rate (fixed)
VIBE_CFG_SCALE = 1.3      # classifier-free guidance; demo default
VIBE_DEFAULT_SPEAKER = "Emma"

# Speaker name -> preset filename in the GitHub demo/voices/streaming_model dir.
# 4 male + 2 female English voices; pick any 4 for a panel.
VIBE_VOICES = {
    "Carter": "en-Carter_man.pt",
    "Davis": "en-Davis_man.pt",
    "Emma": "en-Emma_woman.pt",
    "Frank": "en-Frank_man.pt",
    "Grace": "en-Grace_woman.pt",
    "Mike": "en-Mike_man.pt",
}
VIBE_VOICES_BASE_URL = (
    "https://raw.githubusercontent.com/microsoft/VibeVoice/main/demo/voices/streaming_model"
)


def _norm_engine(engine: str | None) -> str:
    """Only "vibe-rt" (or the legacy "vox") selects VibeVoice; else Piper."""
    e = (engine or "").strip().lower()
    return "vibe-rt" if e in ("vibe-rt", "vibe", "vox") else "piper"


def _norm_speaker(speaker: str | None) -> str:
    """Resolve a panelist name to a known preset, falling back to the default."""
    s = (speaker or "").strip()
    if s in VIBE_VOICES:
        return s
    # Case-insensitive match so the frontend can be loose about casing.
    for name in VIBE_VOICES:
        if name.lower() == s.lower():
            return name
    return VIBE_DEFAULT_SPEAKER


# ── request models ───────────────────────────────────────────────────────────
class TtsRequest(BaseModel):
    text: str
    # "piper" (fast, default) or "vibe-rt" (VibeVoice, humanlike).
    engine: str | None = None
    # Which panelist voice to use (vibe-rt only). One of VIBE_VOICES; ignored by
    # Piper. Lets the interview pick a different voice per panelist per turn.
    speaker: str | None = None


class SttRequest(BaseModel):
    # Base64-encoded WAV (any sample rate / channels; we normalize on read).
    audio_b64: str


# ── lazy singletons ──────────────────────────────────────────────────────────
_lock = threading.Lock()
# Serializes VibeVoice synth. One model on one GPU can't run two generate() calls
# at once (e.g. the warmup overlapping the first interview question) — concurrent
# decodes corrupt each other and yield garbled / no audio. Every vibe-rt synth
# acquires this so they run strictly one at a time.
_synth_lock = threading.Lock()
_state = {
    "device": None,         # "cuda" | "cpu"
    "piper": None,          # PiperVoice (fast default engine)
    "piper_sr": 22050,      # Piper voice output sample rate
    "stt": None,            # faster-whisper WhisperModel
    "import_error": None,   # str if torch/models can't be imported
    "vibe_model": None,     # VibeVoiceStreamingForConditionalGenerationInference
    "vibe_proc": None,      # VibeVoiceStreamingProcessor
    "vibe_presets": {},     # speaker name -> prefilled-output tensor (cached)
    "vibe_warm": False,     # True once a throwaway synth has JIT-compiled kernels
    "vibe_warming": False,  # guards against overlapping warm requests
}


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


# ── VibeVoice loading ────────────────────────────────────────────────────────
def _vibe_voice_dir():
    """Local dir holding the downloaded speaker preset .pt files."""
    from pathlib import Path
    d = Path(__file__).resolve().parent.parent / "models" / "vibevoice" / "voices"
    d.mkdir(parents=True, exist_ok=True)
    return d


def _ensure_voice_pt(speaker: str):
    """Return the local path to a speaker preset, downloading it once.

    Search order: repo models/vibevoice/voices/ (pre-fetched by setup.ps1) →
    lazy-download from the GitHub raw URL on first use."""
    import urllib.request
    fname = VIBE_VOICES[speaker]
    target = _vibe_voice_dir() / fname
    if not target.exists():
        urllib.request.urlretrieve(f"{VIBE_VOICES_BASE_URL}/{fname}", target)
    return target


def _get_vibe():
    """Load VibeVoice-Realtime-0.5B + its processor once. Raises with a helpful
    message if deps are missing."""
    if _state["vibe_model"] is not None:
        return _state["vibe_model"], _state["vibe_proc"]
    with _lock:
        if _state["vibe_model"] is None:
            import torch
            # Heavy; imported lazily so the sidecar boots without the voice stack.
            from vibevoice.modular.modeling_vibevoice_streaming_inference import (
                VibeVoiceStreamingForConditionalGenerationInference,
            )
            from vibevoice.processor.vibevoice_streaming_processor import (
                VibeVoiceStreamingProcessor,
            )
            device = _device()
            dtype = torch.bfloat16 if device == "cuda" else torch.float32
            # Use sdpa, not flash_attention_2: flash-attn isn't installed (no
            # prebuilt Windows wheels), and asking for it makes from_pretrained
            # raise/retry — wasted work. sdpa is built into torch and fast enough.
            model = VibeVoiceStreamingForConditionalGenerationInference.from_pretrained(
                VIBE_MODEL_ID, torch_dtype=dtype, attn_implementation="sdpa",
            )
            model = model.to(device)
            model.eval()
            proc = VibeVoiceStreamingProcessor.from_pretrained(VIBE_MODEL_ID)
            _state["vibe_model"] = model
            _state["vibe_proc"] = proc
    return _state["vibe_model"], _state["vibe_proc"]


def _vibe_prefilled(speaker: str):
    """Load (and cache) the prefilled-output preset for a speaker.

    The preset .pt isn't bare tensors — it's a pickled transformers model output
    (a BaseModelOutputWithPast holding the speaker's prefilled KV cache). Under
    torch's safe loader (`weights_only=True`, the default since torch 2.6) that
    object's classes must be explicitly allowlisted, so we register the small set
    the presets reference rather than fall back to the unsafe `weights_only=False`
    (which would allow arbitrary code execution on load)."""
    if speaker in _state["vibe_presets"]:
        return _state["vibe_presets"][speaker]
    import torch
    # Double-checked locking (mirrors _get_vibe / _get_piper): without it two
    # concurrent first-uses of the same speaker — e.g. the warmup synth and the
    # first interview question — both miss the cache and redundantly download +
    # torch.load the preset (and call add_safe_globals twice).
    with _lock:
        if speaker in _state["vibe_presets"]:
            return _state["vibe_presets"][speaker]
        _allowlist_preset_globals()
        pt = _ensure_voice_pt(speaker)
        prefilled = torch.load(pt, map_location=_device(), weights_only=True)
        _state["vibe_presets"][speaker] = prefilled
        return prefilled


def _allowlist_preset_globals() -> None:
    """Allowlist the transformers classes the VibeVoice presets pickle, so they
    load under torch's safe (`weights_only=True`) unpickler. Best-effort per
    class name so a transformers version that lacks one doesn't break the rest."""
    import torch

    safe = []
    try:
        from transformers.modeling_outputs import BaseModelOutputWithPast
        safe.append(BaseModelOutputWithPast)
    except Exception:
        pass
    try:
        from transformers.cache_utils import Cache, DynamicCache
        safe += [Cache, DynamicCache]
    except Exception:
        pass
    if safe:
        try:
            torch.serialization.add_safe_globals(safe)
        except Exception:
            pass


def _vibe_synth_stream(text: str, speaker: str):
    """Synthesize one line with the chosen preset voice, yielding little-endian
    16-bit mono PCM frames as VibeVoice decodes them (true low-latency stream).

    `model.generate(audio_streamer=...)` runs on a background thread and pushes
    audio chunks into the streamer; we drain `get_stream(0)` here and convert
    each torch chunk to PCM as it lands, so the interviewer starts talking ~0.3s
    in instead of after the whole sentence. The call shape mirrors
    demo/web/app.py and is isolated here in case the package API drifts.
    """
    import copy
    import threading

    import numpy as np
    import torch
    from vibevoice.modular.streamer import AudioStreamer

    model, proc = _get_vibe()
    prefilled = _vibe_prefilled(speaker)
    inputs = proc.process_input_with_cached_prompt(
        text=text.strip(),
        cached_prompt=prefilled,
        padding=True,
        return_tensors="pt",
        return_attention_mask=True,
    )
    device = _device()
    for k, v in list(inputs.items()):
        if hasattr(v, "to"):
            inputs[k] = v.to(device)

    streamer = AudioStreamer(batch_size=1, stop_signal=None, timeout=None)
    stop_event = threading.Event()
    err: dict = {}

    def _run():
        try:
            with torch.no_grad():
                model.generate(
                    **inputs,
                    max_new_tokens=None,
                    cfg_scale=VIBE_CFG_SCALE,
                    tokenizer=proc.tokenizer,
                    generation_config={"do_sample": False},
                    audio_streamer=streamer,
                    stop_check_fn=stop_event.is_set,
                    verbose=False,
                    all_prefilled_outputs=copy.deepcopy(prefilled),
                )
        except Exception as exc:  # re-raised on the consumer side after drain
            err["exc"] = exc
        finally:
            # generate() normally ends the stream itself; end() again defensively
            # so a mid-synth error can't leave the consumer iterator blocked.
            for args in ((), ([0],)):
                try:
                    streamer.end(*args)
                    break
                except Exception:
                    continue

    # Only one VibeVoice generate() at a time (see _synth_lock). A warmup synth
    # overlapping the first real question is the common collision; without this
    # they interleave on the GPU and the audio comes out garbled or empty.
    _synth_lock.acquire()
    thread = threading.Thread(target=_run, daemon=True)
    thread.start()
    try:
        for chunk in streamer.get_stream(0):
            arr = chunk.detach().to(torch.float32).cpu().numpy().reshape(-1)
            arr = np.clip(arr, -1.0, 1.0)
            yield (arr * 32767.0).astype("<i2").tobytes()
    except GeneratorExit:
        # Client disconnected (Rust barge-in / stop) — halt synth, don't leak.
        stop_event.set()
        raise
    finally:
        stop_event.set()
        thread.join(timeout=5.0)
        _synth_lock.release()
    if "exc" in err:
        raise err["exc"]


# ── Piper loading ────────────────────────────────────────────────────────────
def _ensure_piper_model():
    """Return the path to the Piper .onnx, downloading the model + config once.

    Search order: INTERPREP_PIPER_MODEL env → repo `models/piper/` (pre-fetched
    by setup.ps1) → user data dir (lazy-downloaded from HF on first use)."""
    import os
    import urllib.request
    from pathlib import Path

    env = os.environ.get("INTERPREP_PIPER_MODEL")
    if env and Path(env).exists():
        return Path(env)
    repo = Path(__file__).resolve().parent.parent / "models" / "piper" / f"{PIPER_VOICE}.onnx"
    if repo.exists() and Path(str(repo) + ".json").exists():
        return repo

    target = _voice_dir() / f"{PIPER_VOICE}.onnx"
    cfg = Path(str(target) + ".json")
    if not target.exists():
        urllib.request.urlretrieve(f"{PIPER_BASE_URL}/{PIPER_VOICE}.onnx", target)
    if not cfg.exists():
        urllib.request.urlretrieve(f"{PIPER_BASE_URL}/{PIPER_VOICE}.onnx.json", cfg)
    return target


def _get_piper():
    """Load the Piper voice once. Raises with a helpful message if deps missing."""
    if _state["piper"] is not None:
        return _state["piper"]
    with _lock:
        if _state["piper"] is None:
            try:
                from piper import PiperVoice  # piper-tts; light (onnxruntime)
            except ImportError:
                from piper.voice import PiperVoice  # older module layout
            model = _ensure_piper_model()
            use_cuda = _device() == "cuda"
            try:
                voice = PiperVoice.load(str(model), use_cuda=use_cuda)
            except TypeError:
                # Older/newer signatures may not accept use_cuda.
                voice = PiperVoice.load(str(model))
            _state["piper"] = voice
            _state["piper_sr"] = int(getattr(voice.config, "sample_rate", _state["piper_sr"]))
    return _state["piper"]


def _piper_pcm_iter(voice, text: str):
    """Yield int16 little-endian mono PCM bytes from a PiperVoice.

    Works across piper-tts versions: ≤1.2 exposes `synthesize_stream_raw(text)`
    (raw int16 bytes); ≥1.3 yields AudioChunk objects with `audio_int16_bytes`."""
    raw = getattr(voice, "synthesize_stream_raw", None)
    if callable(raw):
        for chunk in raw(text):
            yield bytes(chunk)
        return
    for ch in voice.synthesize(text):
        data = getattr(ch, "audio_int16_bytes", None)
        if data is None:
            import numpy as np
            arr = np.asarray(getattr(ch, "audio_float_array", ch), dtype=np.float32).flatten()
            data = (np.clip(arr, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()
        yield bytes(data)


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
def _pcm16_to_wav(pcm: bytes, sample_rate: int) -> bytes:
    """Wrap raw 16-bit mono PCM bytes in a WAV container."""
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


def _prepare(engine: str, speaker: str) -> int:
    """Load the engine's model (and preset, for vibe-rt) and return its sample
    rate. Called before streaming so the rate is known up front."""
    if engine == "vibe-rt":
        _get_vibe()
        _vibe_prefilled(speaker)
        return VIBE_SR
    _get_piper()
    return int(_state["piper_sr"])


def _synthesize(text: str, engine: str, speaker: str) -> tuple[bytes, int]:
    if engine == "vibe-rt":
        pcm = b"".join(_vibe_synth_stream(text, speaker))
        return _pcm16_to_wav(pcm, VIBE_SR), VIBE_SR
    voice = _get_piper()
    sr = int(_state["piper_sr"])
    pcm = b"".join(_piper_pcm_iter(voice, text))
    return _pcm16_to_wav(pcm, sr), sr


def _synth_stream(text: str, engine: str, speaker: str):
    """Yield raw 16-bit mono PCM frames for the chosen engine as they decode."""
    if engine == "vibe-rt":
        yield from _vibe_synth_stream(text, speaker)
        return
    voice = _get_piper()
    yield from _piper_pcm_iter(voice, text)


def _transcribe(wav_bytes: bytes) -> str:
    model = _get_stt()
    segments, _info = model.transcribe(io.BytesIO(wav_bytes), beam_size=1)
    return "".join(seg.text for seg in segments).strip()


def prewarm() -> None:
    """Load the default (Piper) engine ahead of time, downloading its voice model
    on first ever run.

    Called from the app lifespan in a background thread so the first interview
    question doesn't pay the model-load (and one-time download) cold start.
    Best-effort — silently no-ops if the voice stack isn't installed. VibeVoice
    is NOT warmed here: it's opt-in and heavy, so it loads lazily when toggled on.
    """
    try:
        _get_piper()
    except Exception:
        pass


def warm(engine: str, speaker: str = VIBE_DEFAULT_SPEAKER) -> None:
    """Pay an engine's cold-start cost ahead of the interview.

    For vibe-rt the cold start is brutal on a laptop GPU: the one-time model
    load is slow AND the *first* synth is ~20x real-time while CUDA kernels JIT-
    compile (a warm synth is ~0.3s to first audio). So we load the model and run
    one short throwaway synth here — called when the user enables the humanlike
    voice, so the warmup lands on the toggle instead of the first question.
    Idempotent + single-flight; best-effort (never raises)."""
    engine = _norm_engine(engine)
    if engine != "vibe-rt":
        try:
            _get_piper()
        except Exception:
            pass
        return
    if _state["vibe_warm"] or _state["vibe_warming"]:
        return
    _state["vibe_warming"] = True
    try:
        for _ in _vibe_synth_stream("Hello.", _norm_speaker(speaker)):
            pass
        _state["vibe_warm"] = True
    except Exception:
        pass
    finally:
        _state["vibe_warming"] = False


# ── endpoints ────────────────────────────────────────────────────────────────
@router.get("/status")
async def status():
    """Report whether voice is usable + which device it runs on. `available` is
    the default (Piper + STT) path; `vibe_available` reports whether the heavier
    VibeVoice voice can be toggled on. `voices` lists the panelist presets the UI
    can assign. The UI shows device so the user knows if they're on GPU (live) or
    CPU (laggy)."""
    device = _device()
    detail = _state["import_error"]
    try:
        import importlib.util
        def have(mod: str) -> bool:
            return importlib.util.find_spec(mod) is not None
        piper_ok = have("piper")
        stt_ok = have("faster_whisper")
        vibe_ok = have("torch") and have("vibevoice")
        available = piper_ok and stt_ok
        if not available:
            missing = [m for m, ok in
                       (("piper-tts", piper_ok), ("faster-whisper", stt_ok)) if not ok]
            detail = f"missing: {', '.join(missing)} (run backend/setup.ps1 -Voice)"
    except Exception as exc:
        available = False
        vibe_ok = False
        detail = str(exc)
    return {
        "available": available,
        "device": device,
        "vibe_available": vibe_ok,
        "voices": list(VIBE_VOICES.keys()),
        "default_speaker": VIBE_DEFAULT_SPEAKER,
        "tts_loaded": (_state["piper"] is not None) or (_state["vibe_model"] is not None),
        "stt_loaded": _state["stt"] is not None,
        "detail": detail,
    }


@router.post("/warm")
async def warm_endpoint(req: TtsRequest):
    """Kick an engine's warmup in the background and return immediately. The UI
    calls this when the user turns on the humanlike voice so vibe-rt's slow cold
    start happens then, not on the first interview question."""
    engine = _norm_engine(req.engine)
    speaker = _norm_speaker(req.speaker)
    threading.Thread(target=warm, args=(engine, speaker), daemon=True).start()
    return {"warming": True, "engine": engine}


@router.post("/tts")
async def tts(req: TtsRequest):
    text = (req.text or "").strip()
    if not text:
        return {"error": "empty text"}
    engine = _norm_engine(req.engine)
    speaker = _norm_speaker(req.speaker)
    try:
        wav_bytes, sr = await asyncio.to_thread(_synthesize, text, engine, speaker)
        return {"audio_b64": base64.b64encode(wav_bytes).decode("ascii"), "sample_rate": sr}
    except Exception as exc:
        return {"error": f"TTS failed: {exc}"}


@router.post("/tts_stream")
async def tts_stream(req: TtsRequest):
    """Stream synthesized PCM as it's decoded. Body = raw little-endian 16-bit
    mono PCM frames (no WAV header); the sample rate is in `X-Sample-Rate`."""
    text = (req.text or "").strip()
    if not text:
        return Response(status_code=204)
    engine = _norm_engine(req.engine)
    speaker = _norm_speaker(req.speaker)
    # Load the model (and preset, for vibe-rt) up front so the sample rate is
    # known before we commit to streaming headers, and import/download errors
    # surface as a clean 500 instead of a half-written stream.
    try:
        sr = _prepare(engine, speaker)
    except Exception as exc:
        return JSONResponse({"error": f"TTS unavailable: {exc}"}, status_code=500)

    def gen():
        try:
            yield from _synth_stream(text, engine, speaker)
        except Exception:
            # Mid-stream failure: stop cleanly. Headers are already sent, so the
            # client just sees a short stream — better than a 500 it can't read.
            return

    return StreamingResponse(
        gen(),
        media_type="application/octet-stream",
        headers={"X-Sample-Rate": str(sr)},
    )


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
