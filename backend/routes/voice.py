"""Voice services for the live mock interview and the copilot.

  POST /voice/tts_stream {text,engine,speaker} -> raw PCM16 stream          (any TTS engine)
  POST /voice/tts    {text,engine,speaker} -> {audio_b64, sample_rate}
  POST /voice/stt    {audio_b64}           -> {text}                     (faster-whisper batch)
  POST /voice/asr/{start|chunk|snapshot|finish|close}                    (any recognizer)
  POST /voice/stt-stream/{start|chunk|finish|cancel}                       (Moonshine, legacy)
  GET  /voice/status                       -> capability + device report

One engine layer (see "TTS engine registry" and "Recognizer registry"): every
voice exposes `stream(text, speaker)` → PCM16 frames, every recognizer exposes
`start / push / finish`, Settings picks the voice, and /voice/status reports
what is installed and loaded. The Phase 6 Mac speech server serves the same
contract. Three TTS engines:
  * "kokoro" (default with a GPU) — Kokoro-82M, natural-sounding and ~25×
    real time on the GPU (0.6–1.9 s a sentence on the CPU, hence not the
    default there); several preset voices, so a panel works.
  * "piper" — fast neural TTS via onnxruntime on the CPU, ~60MB voice model.
    The fallback whenever another engine is missing or fails to load.
  * "vibe-rt" — Microsoft VibeVoice-Realtime-0.5B, the most humanlike. Six
    preset voices (VIBE_VOICES); torch-based, wants a GPU, slow cold start.

Everything is lazy-loaded and degrades to a clear error (or to Piper) instead of
crashing the sidecar at import time. Device is chosen automatically — CUDA when
available, else CPU — and surfaced via /voice/status. Only the chosen engine is
loaded.
"""
from __future__ import annotations

import asyncio
import base64
import io
import os
import sys
import threading
import time
import uuid
import wave

from fastapi import APIRouter
from fastapi.responses import JSONResponse, Response, StreamingResponse
from pydantic import BaseModel

router = APIRouter()

# Engine names, in Settings order. A request naming none of these (or "") gets
# `_default_engine()`: Kokoro with a GPU, else Piper.
TTS_ENGINES = ("kokoro", "piper", "vibe-rt")
FALLBACK_ENGINE = "piper"

# ── Kokoro-82M ───────────────────────────────────────────────────────────────
# Weights (~330MB) + one ~0.5MB style vector per voice come from the HF repo on
# first use (pre-fetched by setup.ps1). misaki's English G2P needs spaCy's
# en_core_web_sm, which it pip-installs itself if missing.
KOKORO_REPO = "hexgrad/Kokoro-82M"
KOKORO_SR = 24000
# Panel presets, American English (lang "a"): display name -> voice id. Ordered
# so a panel of N alternates female/male, best-graded first (the repo's
# VOICES.md grades af_heart A, af_bella A-, the others C+).
KOKORO_VOICES = {
    "Heart": "af_heart",
    "Michael": "am_michael",
    "Bella": "af_bella",
    "Fenrir": "am_fenrir",
    "Sarah": "af_sarah",
    "Puck": "am_puck",
}
KOKORO_DEFAULT_SPEAKER = "Heart"

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
    """Canonical engine name; "" or unknown = the default engine. ("vox" and
    "vibe" are legacy spellings of "vibe-rt".)"""
    e = (engine or "").strip().lower()
    if e in ("vibe-rt", "vibe", "vox"):
        return "vibe-rt"
    return e if e in TTS_ENGINES else _default_engine()


def _pick_speaker(speaker: str | None, voices: dict, default: str) -> str:
    """Resolve a panelist name against one engine's presets (case-insensitive),
    falling back to that engine's default voice."""
    s = (speaker or "").strip().lower()
    for name in voices:
        if name.lower() == s:
            return name
    return default


def _norm_speaker(speaker: str | None) -> str:
    """VibeVoice preset for a panelist name."""
    return _pick_speaker(speaker, VIBE_VOICES, VIBE_DEFAULT_SPEAKER)


# ── request models ───────────────────────────────────────────────────────────
class TtsRequest(BaseModel):
    text: str
    # "kokoro" | "piper" | "vibe-rt"; omitted/"" = the default engine.
    engine: str | None = None
    # Which panelist voice to use: a preset name of the chosen engine
    # (KOKORO_VOICES / VIBE_VOICES); ignored by Piper. Lets the interview pick
    # a different voice per panelist per turn.
    speaker: str | None = None
    # /prepare only: every panel voice, so all of them load behind the modal.
    speakers: list[str] | None = None
    # "moonshine" for the mock-interview candidate stream; omitted by the
    # copilot, which still warms faster-whisper for system-audio transcription.
    stt_engine: str | None = None


class SttRequest(BaseModel):
    # Base64-encoded WAV (any sample rate / channels; we normalize on read).
    audio_b64: str


class VadRequest(BaseModel):
    # Base64-encoded RAW little-endian 16-bit mono PCM at 16 kHz (no WAV header)
    # — the rolling audio tail the capture loop wants a speech/no-speech verdict
    # for. Raw PCM keeps the 4×/s hot path allocation-light on both sides.
    audio_b64: str


class SttStreamChunkRequest(BaseModel):
    session_id: str
    # RAW little-endian mono PCM16. Moonshine accepts arbitrary sample rates.
    audio_b64: str
    sample_rate: int
    elapsed_ms: float


class SttStreamSessionRequest(BaseModel):
    session_id: str


class AsrStartRequest(BaseModel):
    # A recognizer in _ASR ("whisper" | "moonshine"); omitted = "whisper".
    engine: str | None = None


class AsrChunkRequest(BaseModel):
    session_id: str
    # RAW little-endian mono PCM16 (Whisper: 16 kHz, Rust resamples). May be
    # empty when the call only carries `decode`.
    audio_b64: str = ""
    # Whisper: Rust's VAD saw a pause — decode everything received so far.
    decode: bool = False
    # Moonshine: the chunk's rate and the stream clock at its end.
    sample_rate: int = 16000
    elapsed_ms: float = 0.0


class AsrSnapshotRequest(BaseModel):
    session_id: str
    # Samples (16 kHz, from session start) up to the last voiced frame; any
    # decode covering at least this much audio is reused.
    upto_samples: int = 0


# ── lazy singletons ──────────────────────────────────────────────────────────
_lock = threading.Lock()
# Serializes VibeVoice synth. One model on one GPU can't run two generate() calls
# at once (e.g. the warmup overlapping the first interview question) — concurrent
# decodes corrupt each other and yield garbled / no audio. Every vibe-rt synth
# acquires this so they run strictly one at a time.
_synth_lock = threading.Lock()
# Serializes prepare() — see its docstring. Distinct from _lock (model loads)
# and _synth_lock (vibe synth) so a long warm doesn't block unrelated paths.
_prepare_lock = threading.Lock()
# One faster-whisper decode at a time: the warmup, /voice/stt and the copilot's
# streaming sessions all share one WhisperModel.
_stt_infer_lock = threading.Lock()
# Kokoro's load, and one synth chunk at a time (G2P + model share state).
_kokoro_lock = threading.Lock()
_kokoro_synth_lock = threading.Lock()
# One-time cuDNN pinning (see _pin_torch_cudnn) — before ANY ctranslate2 import.
_pin_lock = threading.Lock()
# Builds of torch TTS models, one at a time. transformers' from_pretrained
# (VibeVoice) puts torch in "init on the meta device" mode PROCESS-WIDE while it
# runs, so a Kokoro KModel built at the same moment on another thread came out
# with empty meta weights and failed to load ("v_in is on meta") — Piper then
# spoke in its place (observed 2026-10-02, switching voices before an interview).
_torch_build_lock = threading.Lock()
_state = {
    "device": None,         # "cuda" | "cpu"
    "piper": None,          # PiperVoice (fast fallback engine)
    "piper_sr": 22050,      # Piper voice output sample rate
    "kokoro": None,         # kokoro.KPipeline (model + misaki G2P)
    "kokoro_device": None,
    "kokoro_warm": False,
    "tts_errors": {},       # engine -> load/warm error; such engines fall back to Piper
    "stt": None,            # faster-whisper WhisperModel
    "moonshine": None,      # shared Moonshine Transcriber (per-turn streams)
    "moonshine_warm": False,
    # faster-whisper readiness for /voice/status (the copilot overlay shows it):
    # "idle" → "loading" → "warming" → "ready", or "error".
    "stt_phase": "idle",
    "import_error": None,   # str if torch/models can't be imported
    "vibe_model": None,     # VibeVoiceStreamingForConditionalGenerationInference
    "vibe_proc": None,      # VibeVoiceStreamingProcessor
    "vibe_presets": {},     # speaker name -> prefilled-output tensor (cached)
    "vibe_warm": False,     # True once a throwaway synth has JIT-compiled kernels
}

# Moonshine streams are stateful and receive ordered chunks from one Rust
# capture worker. The model is shared, but each answer owns an independent
# stream/listener. Sessions are short-lived and evicted defensively if a client
# disappears without sending finish/cancel.
_moonshine_lock = threading.Lock()
# moonshine-voice shares one native Transcriber across streams. Keep native
# operations single-flight until concurrent Windows inference is explicitly
# validated; HTTP/session bookkeeping remains concurrent around this lock.
_moonshine_inference_lock = threading.Lock()
MOONSHINE_SESSION_TTL_S = 180.0
# The library recommends 500ms. Smaller values repeatedly invoke the decoder
# faster than this Windows CPU can keep up, creating a backlog that defeats
# streaming; 500ms keeps inference near real-time while phrase completion still
# arrives well before the conservative 1.4s endpoint.
MOONSHINE_UPDATE_INTERVAL_S = 0.5


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
            with _torch_build_lock:
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
            # Only ask onnxruntime for CUDA when its CUDA provider is actually
            # installed (the plain `onnxruntime` wheel is CPU-only). Requesting
            # a missing provider sent ORT through a ~80s probe-and-fallback at
            # load (observed in the field, blocking the "Preparing engine…"
            # modal) — and Piper is faster than real-time on CPU regardless.
            # Provider check first: `_device()` imports torch (~36s cold), which
            # the CPU-only wheel never needs.
            use_cuda = False
            try:
                import onnxruntime
                use_cuda = ("CUDAExecutionProvider" in onnxruntime.get_available_providers()
                            and _device() == "cuda")
            except Exception:
                pass
            t0 = time.time()
            try:
                voice = PiperVoice.load(str(model), use_cuda=use_cuda)
            except TypeError:
                # Older/newer signatures may not accept use_cuda.
                voice = PiperVoice.load(str(model))
            print(f"[voice] piper loaded (cuda={use_cuda}) in {time.time() - t0:.1f}s",
                  flush=True)
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


# ── Kokoro loading ───────────────────────────────────────────────────────────
def _get_kokoro():
    """Load Kokoro-82M once, on CUDA when torch sees a GPU
    (INTERPREP_TTS_DEVICE=cpu|cuda overrides). One KPipeline holds the model
    and misaki's English G2P; voices load into it on first use."""
    if _state["kokoro"] is not None:
        return _state["kokoro"]
    with _kokoro_lock:
        if _state["kokoro"] is None:
            t0 = time.time()
            from kokoro import KModel, KPipeline  # imports torch + spaCy: heavy, lazy
            device = os.environ.get("INTERPREP_TTS_DEVICE", "").strip().lower()
            if device not in ("cpu", "cuda") or (device == "cuda" and _device() != "cuda"):
                device = _device()
            config, weights = _kokoro_file("config.json"), _kokoro_file("kokoro-v1_0.pth")
            with _torch_build_lock:
                model = KModel(repo_id=KOKORO_REPO, config=config, model=weights)
            model = model.to(device).eval()
            _state["kokoro"] = KPipeline(lang_code="a", repo_id=KOKORO_REPO, model=model)
            _state["kokoro_device"] = device
            print(f"[voice] kokoro loaded on {device} in {time.time() - t0:.1f}s", flush=True)
    return _state["kokoro"]


def _kokoro_file(filename: str) -> str:
    """Local path of a file in the Kokoro repo: straight from the HF cache when
    it's there (no network round-trip, works offline), else downloaded."""
    from huggingface_hub import hf_hub_download, try_to_load_from_cache
    hit = try_to_load_from_cache(KOKORO_REPO, filename)
    return hit if isinstance(hit, str) else hf_hub_download(KOKORO_REPO, filename)


def _kokoro_voice(speaker: str | None) -> str:
    """Local .pt path of a panelist's voice (KPipeline caches it by path)."""
    vid = KOKORO_VOICES[_pick_speaker(speaker, KOKORO_VOICES, KOKORO_DEFAULT_SPEAKER)]
    paths = _state.setdefault("kokoro_voice_paths", {})
    if vid not in paths:
        paths[vid] = _kokoro_file(f"voices/{vid}.pt")
    return paths[vid]


def _kokoro_stream(text: str, speaker: str):
    """PCM16 frames for one line. Kokoro isn't autoregressive: each chunk
    (a whole sentence; misaki only splits past 510 phonemes) arrives at once.
    The lock covers only the synthesis step, never a yield, so a stalled
    consumer can't block the next line."""
    import numpy as np
    pipe = _get_kokoro()
    results = pipe(text.strip(), voice=_kokoro_voice(speaker), split_pattern=None)
    while True:
        with _kokoro_synth_lock:
            result = next(results, None)
        if result is None:
            return
        if result.audio is None:
            continue
        arr = np.clip(result.audio.detach().float().cpu().numpy().reshape(-1), -1.0, 1.0)
        yield (arr * 32767.0).astype("<i2").tobytes()


def _pin_torch_cudnn() -> None:
    """Preload torch's bundled cuDNN DLLs before ctranslate2 can load its own.

    ctranslate2 (faster-whisper's backend) ships a LONE cudnn64_9.dll. If STT
    loads first — the normal prepare() order — Windows registers that copy,
    and when VibeVoice's torch stack later asks cuDNN for symbols the lone
    DLL can't serve without its companion DLLs, the process HARD-CRASHES
    mid-synth ("Could not load symbol cudnnGetLibConfig. Error code 127" —
    kills the whole sidecar, observed 2026-07-07). Loading torch's complete
    cuDNN set first makes the loader dedupe by module name so both libraries
    share the good copies. Best-effort no-op when torch isn't installed.

    Finds torch's lib dir WITHOUT importing torch: the import costs tens of
    seconds on a cold boot and the copilot's speech path never needs it.

    EVERY path that imports ctranslate2 / faster_whisper calls this first
    (`_stt_device`, `_get_stt`, `_vad_tail`): a /voice/status poll importing
    ctranslate2 while the Whisper warm-up was still pinning let ctranslate2's
    copy win, and Kokoro's first synth then killed the sidecar with the error
    above (2026-10-01). Once per process; callers block until it's done."""
    with _pin_lock:
        if _state.get("cudnn_pinned"):
            return
        _state["cudnn_pinned"] = True
        try:
            import ctypes
            import glob as _glob
            import importlib.util
            spec = importlib.util.find_spec("torch")
            if spec is None or not spec.submodule_search_locations:
                return
            lib = os.path.join(list(spec.submodule_search_locations)[0], "lib")
            if not os.path.isdir(lib):
                return
            # Same search path torch registers on import, so the cuDNN DLLs'
            # own dependencies (cuBLAS etc.) resolve from torch's copies too.
            _state["torch_dll_dir"] = os.add_dll_directory(lib)
            for dll in sorted(_glob.glob(os.path.join(lib, "cudnn*.dll"))):
                try:
                    ctypes.WinDLL(dll)
                except OSError:
                    pass
        except Exception:
            pass


def _stt_device() -> str:
    """CUDA when CTranslate2 (faster-whisper's backend) sees a GPU. Asked of
    CTranslate2 rather than torch (`_device`) so loading speech recognition
    never imports torch."""
    if _state.get("ct2_device"):
        return _state["ct2_device"]
    _pin_torch_cudnn()  # MUST precede any ctranslate2 import
    try:
        import ctranslate2
        _state["ct2_device"] = "cuda" if ctranslate2.get_cuda_device_count() > 0 else "cpu"
    except Exception:
        _state["ct2_device"] = "cpu"
    return _state["ct2_device"]


def _get_stt():
    """Load faster-whisper once, on the auto-detected device."""
    if _state["stt"] is not None:
        return _state["stt"]
    with _lock:
        if _state["stt"] is None:
            import os
            _state["stt_phase"] = "loading"
            t0 = time.time()
            try:
                _pin_torch_cudnn()  # MUST precede the ctranslate2 import below
                from faster_whisper import WhisperModel
                # STT runs on the GPU when one exists — ~4× faster decode, which is
                # the biggest chunk of question→answer latency on CPU (~3s for a
                # long question vs <1s on GPU). Historical note: this used to
                # default to CPU because loading whisper alongside VibeVoice
                # "froze the app" — that freeze was actually the cuDNN DLL clash
                # fixed in _pin_torch_cudnn (both stacks now verified coexisting on
                # one GPU), and in practice the capture→STT→LLM→TTS cycle is
                # sequential so they don't contend per-turn anyway. Force with
                # INTERPREP_STT_DEVICE=cpu|cuda if a specific box misbehaves.
                device = os.environ.get("INTERPREP_STT_DEVICE", "auto").strip().lower()
                if device not in ("cpu", "cuda"):
                    device = _stt_device()  # auto: cuda when available
                if device == "cuda" and _stt_device() != "cuda":
                    device = "cpu"
                compute = "float16" if device == "cuda" else "int8"
                # English-only "base.en" — faster AND more accurate than multilingual
                # "base" for English interviews. Override with INTERPREP_STT_MODEL
                # (e.g. "tiny.en" for max speed, "small.en" if transcripts are weak).
                model_name = os.environ.get("INTERPREP_STT_MODEL", "base.en")
                _state["stt"] = WhisperModel(
                    model_name, device=device, compute_type=compute, cpu_threads=0,
                )
            except Exception:
                _state["stt_phase"] = "error"
                raise
            _state["stt_device"] = device
            if not _state.get("stt_warm"):
                _state["stt_phase"] = "loaded"
            print(f"[voice] STT loaded: model={model_name} device={device} "
                  f"({time.time() - t0:.1f}s incl. imports)", flush=True)
    return _state["stt"]


def _get_moonshine():
    """Load the shared English Tiny Streaming transcriber once.

    Tiny stays ahead of real-time on the target Windows CPU (local benchmark:
    2.7s inference for 3.7s audio); Small took 11s and created a backlog. Unlike
    Whisper, streaming caches work while the candidate is talking. Model files
    are cached by moonshine-voice after the first download.
    """
    if _state["moonshine"] is not None:
        return _state["moonshine"]
    with _moonshine_lock:
        if _state["moonshine"] is None:
            from moonshine_voice import ModelArch, Transcriber, get_model_for_language
            t0 = time.time()
            model_path, model_arch = get_model_for_language(
                "en", ModelArch.TINY_STREAMING
            )
            _state["moonshine"] = Transcriber(
                model_path=model_path,
                model_arch=model_arch,
                update_interval=MOONSHINE_UPDATE_INTERVAL_S,
            )
            print(
                f"[voice] Moonshine loaded: tiny-streaming-en "
                f"({time.time() - t0:.1f}s)",
                flush=True,
            )
    return _state["moonshine"]


class _MoonshineSession:
    """One candidate answer streamed through the shared Moonshine model."""

    def __init__(self, stream):
        self.stream = stream
        # add_audio synchronously emits listener callbacks, so this must be
        # re-entrant: chunk() owns it while _on_event() updates state.
        self.lock = threading.RLock()
        self.completed: list[str] = []
        self.completed_ids: set[int] = set()
        self.partial = ""
        self.completion_seq = 0
        self.completion_audio_ms = 0.0
        self.submitted_audio_ms = 0.0
        self.touched = time.monotonic()
        stream.add_listener(self._on_event)

    def _on_event(self, event) -> None:
        line = getattr(event, "line", None)
        if line is None:
            return
        text = (getattr(line, "text", "") or "").strip()
        kind = type(event).__name__
        with self.lock:
            self.touched = time.monotonic()
            if text:
                self.partial = text
            if kind == "LineCompleted":
                line_id = int(getattr(line, "line_id", id(line)))
                if line_id not in self.completed_ids:
                    self.completed_ids.add(line_id)
                    if text:
                        self.completed.append(text)
                    self.completion_seq += 1
                    self.completion_audio_ms = self.submitted_audio_ms
                self.partial = ""

    def add_pcm16(self, pcm: bytes, sample_rate: int, elapsed_ms: float) -> dict:
        import numpy as np
        samples = np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768.0
        with self.lock:
            self.submitted_audio_ms = max(self.submitted_audio_ms, elapsed_ms)
            self.touched = time.monotonic()
            before = self.completion_seq
            if samples.size:
                with _moonshine_inference_lock:
                    self.stream.add_audio(samples, max(1, sample_rate))
            return {
                "healthy": True,
                "phrase_completed": self.completion_seq > before,
                "completion_seq": self.completion_seq,
                "completion_audio_ms": self.completion_audio_ms,
                "partial": self.partial,
            }

    def finish(self) -> str:
        with self.lock:
            try:
                with _moonshine_inference_lock:
                    transcript = self.stream.stop()
                # stop() emits LineCompleted, but also inspect its returned
                # snapshot for versions that coalesce listener notifications.
                for line in getattr(transcript, "lines", []) if transcript else []:
                    text = (getattr(line, "text", "") or "").strip()
                    line_id = int(getattr(line, "line_id", id(line)))
                    if text and line_id not in self.completed_ids:
                        self.completed_ids.add(line_id)
                        self.completed.append(text)
                parts = self.completed.copy()
                if self.partial and (not parts or parts[-1] != self.partial):
                    parts.append(self.partial)
                return " ".join(parts).strip()
            finally:
                with _moonshine_inference_lock:
                    self.stream.close()

    def cancel(self) -> None:
        with self.lock:
            try:
                with _moonshine_inference_lock:
                    self.stream.stop()
            except Exception:
                pass
            finally:
                with _moonshine_inference_lock:
                    self.stream.close()


def _moonshine_open() -> _MoonshineSession:
    transcriber = _get_moonshine()
    with _moonshine_inference_lock:
        stream = transcriber.create_stream(
            update_interval=MOONSHINE_UPDATE_INTERVAL_S
        )
    session = _MoonshineSession(stream)
    with _moonshine_inference_lock:
        stream.start()
    return session


# ── Copilot streaming ASR (faster-whisper sessions) ──────────────────────────
# The copilot's Rust capture loop streams the interviewer's audio here as raw
# 16 kHz PCM16 while they talk (Rust runs Silero VAD, so a session only ever
# holds one question). Whisper re-decodes the growing buffer in the background
# for live partials, and again the moment Rust reports a pause — so when the
# endpoint fires ~150 ms later the transcript is usually already decoded and
# `snapshot` returns it without touching the GPU. A warm base.en decode is
# ~60 ms on a laptop GPU; the old batch path (WAV + PyAV decode + HTTP) was
# ~375 ms of dead air after every question.
ASR_SR = 16000
ASR_PARTIAL_EVERY_S = 0.7   # new audio between background partial decodes
ASR_MIN_DECODE_S = 0.4      # too little audio to be worth a partial
ASR_MAX_S = 45.0            # Rust caps a question at 28 s; slack for preroll
ASR_SESSION_TTL_S = 120.0
_asr_pool = None            # one worker = one GPU decode at a time, FIFO
_asr_pool_lock = threading.Lock()


def _asr_executor():
    global _asr_pool
    with _asr_pool_lock:
        if _asr_pool is None:
            from concurrent.futures import ThreadPoolExecutor
            _asr_pool = ThreadPoolExecutor(max_workers=1, thread_name_prefix="asr")
        return _asr_pool


def _decode_pcm(audio) -> str:
    """One greedy decode of 16 kHz float32 audio (same settings as
    `_transcribe`, minus WAV parsing and timestamps)."""
    model = _get_stt()
    with _stt_infer_lock:
        segments, _info = model.transcribe(
            audio, beam_size=1, vad_filter=True, language="en",
            condition_on_previous_text=False, temperature=0.0,
            without_timestamps=True,
        )
        return "".join(seg.text for seg in segments).strip()


class _AsrSession:
    """One interviewer question. `partial` is the newest finished decode and
    `partial_samples` how much audio it covered; decodes run on the shared
    one-thread pool, newest-wins."""

    def __init__(self):
        import numpy as np
        self.lock = threading.Lock()
        self.audio = np.zeros(0, dtype=np.float32)
        self.partial = ""
        self.partial_samples = 0
        self.pending = None          # Future of the newest scheduled decode
        self.pending_samples = 0
        self.touched = time.monotonic()

    def append(self, pcm: bytes) -> None:
        import numpy as np
        if not pcm:
            return
        chunk = np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768.0
        with self.lock:
            room = int(ASR_MAX_S * ASR_SR) - self.audio.size
            if room > 0:
                self.audio = np.concatenate([self.audio, chunk[:room]])
            self.touched = time.monotonic()

    def _busy(self) -> bool:
        return self.pending is not None and not self.pending.done()

    def _schedule(self):
        """Queue a decode of everything received so far. Caller holds lock."""
        audio, n = self.audio, self.audio.size   # arrays are only ever replaced
        fut = _asr_executor().submit(self._run, audio, n)
        self.pending, self.pending_samples = fut, n
        return fut

    def _run(self, audio, n: int) -> str:
        text = _decode_pcm(audio)
        with self.lock:
            if n >= self.partial_samples:
                self.partial, self.partial_samples = text, n
        return text

    def poke(self, force: bool) -> None:
        """`force` (Rust saw a pause) decodes now unless a decode already
        covers every sample; otherwise refresh the partial every
        ASR_PARTIAL_EVERY_S of new audio, never stacking background decodes."""
        with self.lock:
            n = self.audio.size
            if n < ASR_MIN_DECODE_S * ASR_SR:
                return
            if force:
                if not (self._busy() and self.pending_samples >= n) and self.partial_samples < n:
                    self._schedule()
                return
            if not self._busy() and n - self.partial_samples >= ASR_PARTIAL_EVERY_S * ASR_SR:
                self._schedule()

    def state(self) -> dict:
        with self.lock:
            return {"partial": self.partial, "partial_samples": self.partial_samples,
                    "samples": int(self.audio.size)}

    def snapshot(self, upto: int) -> tuple[str, bool]:
        """Transcript covering at least `upto` samples (the last voiced sample
        Rust saw). Reuses a finished or in-flight decode when one covers it."""
        with self.lock:
            upto = min(max(upto, 1), self.audio.size)
            if self.partial_samples >= upto:
                return self.partial, True
            if self.pending is not None and self.pending_samples >= upto:
                fut, reused = self.pending, True
            else:
                fut, reused = self._schedule(), False
        return fut.result(timeout=30), reused


# ── Recognizer registry ──────────────────────────────────────────────────────
# Every recognizer exposes one session contract, shared with the Phase 6 Mac
# speech server: open() → session, push(session, pcm16, …) → live state,
# finish(session) → final text, cancel(session). Sessions of every engine live
# in one table, so /voice/asr/* serves any engine; /voice/stt-stream/* stays as
# the Moonshine spelling the mock-interview mic already speaks.
_have_cache: dict[str, bool] = {}


def _have(module: str) -> bool:
    """Is `module` installed? Checked once per process without importing it
    (every TTS request resolves its engine through this)."""
    if module not in _have_cache:
        import importlib.util
        try:
            _have_cache[module] = importlib.util.find_spec(module) is not None
        except Exception:
            _have_cache[module] = False
    return _have_cache[module]


class _WhisperAsr:
    """faster-whisper streaming sessions (copilot system audio). PCM must be
    16 kHz; `decode=True` (Rust's pause hint) decodes everything so far."""
    name = "whisper"
    ttl_s = ASR_SESSION_TTL_S

    def available(self) -> bool:
        return _have("faster_whisper")

    def loaded(self) -> bool:
        return _state["stt"] is not None

    def device(self) -> str | None:
        return _state.get("stt_device")

    def open(self) -> _AsrSession:
        return _AsrSession()

    def push(self, s: _AsrSession, pcm: bytes, sample_rate: int = ASR_SR,
             elapsed_ms: float = 0.0, decode: bool = False) -> dict:
        s.append(pcm)
        s.poke(force=decode)
        return s.state()

    def finish(self, s: _AsrSession) -> str:
        return s.snapshot(s.state()["samples"])[0]

    def cancel(self, s: _AsrSession) -> None:
        pass


class _MoonshineAsr:
    """Moonshine Tiny streaming on the CPU (mock-interview mic). Accepts any
    sample rate; reports phrase completions the mic's endpointing uses."""
    name = "moonshine"
    ttl_s = MOONSHINE_SESSION_TTL_S

    def available(self) -> bool:
        return _have("moonshine_voice")

    def loaded(self) -> bool:
        return _state["moonshine"] is not None

    def device(self) -> str | None:
        return "cpu"

    def open(self) -> _MoonshineSession:
        return _moonshine_open()

    def push(self, s: _MoonshineSession, pcm: bytes, sample_rate: int = ASR_SR,
             elapsed_ms: float = 0.0, decode: bool = False) -> dict:
        return s.add_pcm16(pcm, sample_rate, elapsed_ms)

    def finish(self, s: _MoonshineSession) -> str:
        return s.finish()

    def cancel(self, s: _MoonshineSession) -> None:
        s.cancel()


_ASR = {e.name: e for e in (_WhisperAsr(), _MoonshineAsr())}
_asr_sessions_lock = threading.Lock()
_asr_sessions: dict[str, tuple] = {}   # session id -> (engine, session)


def _asr_evict_stale() -> None:
    now = time.monotonic()
    with _asr_sessions_lock:
        stale = [sid for sid, (eng, s) in _asr_sessions.items()
                 if now - s.touched > eng.ttl_s]
        dropped = [_asr_sessions.pop(sid) for sid in stale]
    for eng, s in dropped:
        try:
            eng.cancel(s)
        except Exception:
            pass


def _asr_open(engine: str) -> str:
    eng = _ASR.get(engine)
    if eng is None:
        raise ValueError(f"unknown recognizer {engine!r}")
    _asr_evict_stale()
    session = eng.open()
    session_id = uuid.uuid4().hex
    with _asr_sessions_lock:
        _asr_sessions[session_id] = (eng, session)
    return session_id


def _asr_get(session_id: str) -> tuple:
    with _asr_sessions_lock:
        entry = _asr_sessions.get(session_id)
    if entry is None:
        raise ValueError("unknown or expired ASR session")
    return entry


def _asr_end(session_id: str, cancel: bool = False) -> str:
    """Finish (or cancel) and forget a session; returns the final text."""
    with _asr_sessions_lock:
        entry = _asr_sessions.pop(session_id, None)
    if entry is None:
        raise ValueError("unknown or expired ASR session")
    eng, session = entry
    if cancel:
        eng.cancel(session)
        return ""
    return eng.finish(session)


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


# ── TTS engine registry ──────────────────────────────────────────────────────
# Every interviewer voice implements one contract, shared with the Phase 6 Mac
# speech server:
#   available()        deps installed (find_spec only — never imports them)
#   load(speakers)     model + the panel's voices, blocking, idempotent;
#                      returns the output sample rate
#   warm(speakers)     load + one throwaway line, so the first real one is warm
#   stream(text, spk)  little-endian PCM16 mono frames as they're synthesized
# An engine that's missing or failed to load/warm falls back to Piper
# (`_resolve_engine`), so a broken install never silences the interviewer.
class _PiperTts:
    name, label = "piper", "Piper"
    voices: dict = {}
    default_speaker = ""

    def available(self) -> bool:
        return _have("piper")

    def loaded(self) -> bool:
        return _state["piper"] is not None

    def warmed(self) -> bool:
        return self.loaded()

    def device(self) -> str:
        return "cpu"

    def load(self, speakers=()) -> int:
        _get_piper()
        return int(_state["piper_sr"])

    def warm(self, speakers=()) -> None:
        self.load()

    def stream(self, text: str, speaker: str = ""):
        yield from _piper_pcm_iter(_get_piper(), text)


class _KokoroTts:
    name, label = "kokoro", "Kokoro"
    voices = KOKORO_VOICES
    default_speaker = KOKORO_DEFAULT_SPEAKER

    def available(self) -> bool:
        return _have("kokoro") and _have("misaki") and _have("torch")

    def loaded(self) -> bool:
        return _state["kokoro"] is not None

    def warmed(self) -> bool:
        return bool(_state["kokoro_warm"])

    def device(self) -> str | None:
        return _state["kokoro_device"]

    def load(self, speakers=()) -> int:
        pipe = _get_kokoro()
        for spk in speakers or (self.default_speaker,):
            path = _kokoro_voice(spk)
            with _kokoro_synth_lock:
                pipe.load_single_voice(path)
        return KOKORO_SR

    def warm(self, speakers=()) -> None:
        self.load(speakers)
        # Waits out a warm already in flight (the modal-open head start), so
        # /prepare never reports ready while the first synth is still cold.
        with _warm_locks[self.name]:
            if _state["kokoro_warm"]:
                return
            t0 = time.time()
            # Three lengths: the first CUDA synth pays one-time setup (~1.5s),
            # each new input length a little more (~0.1s).
            for line in ("Hello.",
                         "Thanks for joining me today, let's get started.",
                         "To begin, could you walk me through your background and the "
                         "projects that best prepared you for this role?"):
                for _ in _kokoro_stream(line, speakers[0] if speakers else ""):
                    pass
            _state["kokoro_warm"] = True
            print(f"[voice] kokoro warmed in {time.time() - t0:.1f}s", flush=True)

    def stream(self, text: str, speaker: str = ""):
        yield from _kokoro_stream(text, speaker)


class _VibeTts:
    name, label = "vibe-rt", "VibeVoice"
    voices = VIBE_VOICES
    default_speaker = VIBE_DEFAULT_SPEAKER

    def available(self) -> bool:
        return _have("torch") and _have("vibevoice")

    def loaded(self) -> bool:
        return _state["vibe_model"] is not None

    def warmed(self) -> bool:
        return bool(_state["vibe_warm"])

    def device(self) -> str | None:
        return _state["device"] if self.loaded() else None

    def load(self, speakers=()) -> int:
        _get_vibe()
        for spk in speakers or (self.default_speaker,):
            _vibe_prefilled(_norm_speaker(spk))
        return VIBE_SR

    def warm(self, speakers=()) -> None:
        # vibe-rt's cold start is brutal on a laptop GPU: the one-time model
        # load is slow AND the first synth is ~20× real time while CUDA kernels
        # JIT-compile (a warm synth is ~0.3s to first audio). Single-flight; a
        # second caller waits for the first instead of returning cold.
        self.load(speakers)
        with _warm_locks[self.name]:
            if _state["vibe_warm"]:
                return
            for _ in _vibe_synth_stream("Hello.", _norm_speaker(speakers[0] if speakers else "")):
                pass
            _state["vibe_warm"] = True

    def stream(self, text: str, speaker: str = ""):
        yield from _vibe_synth_stream(text, _norm_speaker(speaker))


_TTS = {e.name: e for e in (_KokoroTts(), _PiperTts(), _VibeTts())}
_warm_locks = {name: threading.Lock() for name in _TTS}


def _default_engine() -> str:
    """Kokoro when installed, not broken, and a GPU is present, else Piper.
    On this laptop's CPU Kokoro needs 0.6–1.9 s per sentence (RTF ~0.3) vs
    Piper's ~0.15 s; on the RTX 4050 it beats Piper (p50 114 vs 156 ms)."""
    k = _TTS["kokoro"]
    if k.available() and k.name not in _state["tts_errors"] and _stt_device() == "cuda":
        return "kokoro"
    return FALLBACK_ENGINE


def _resolve_engine(engine: str | None):
    """The engine to use for a request: the named one, or Piper when that one
    is missing or already failed to load."""
    eng = _TTS[_norm_engine(engine)]
    if eng.name != FALLBACK_ENGINE and (not eng.available() or eng.name in _state["tts_errors"]):
        return _TTS[FALLBACK_ENGINE]
    return eng


def _load_engine(engine: str | None, speakers=()) -> tuple:
    """Load an engine (falling back to Piper if it can't load). Returns
    (engine, sample_rate)."""
    eng = _resolve_engine(engine)
    try:
        return eng, eng.load(speakers)
    except Exception as exc:
        if eng.name == FALLBACK_ENGINE:
            raise
        _state["tts_errors"][eng.name] = str(exc)
        print(f"[voice] {eng.name} failed to load, falling back to Piper: {exc}", flush=True)
        fb = _TTS[FALLBACK_ENGINE]
        return fb, fb.load()


def _transcribe(wav_bytes: bytes) -> str:
    model = _get_stt()
    # vad_filter drops non-speech (leading/trailing silence + mid-question think
    # pauses) before decoding, so a long question with pauses transcribes faster
    # and doesn't emit hallucinated text over the silent stretches.
    # language pinned + condition_on_previous_text off: skips language detection
    # and the per-window prompt threading — measurably faster on long clips and
    # less prone to repetition-loop hallucinations.
    # temperature pinned to a single pass: the default fallback ladder re-decodes
    # any segment whose compression ratio looks off at up to 5 higher
    # temperatures, which multiplied decode time 3-6× on real loopback captures
    # (observed 6.7s for one question). A single greedy pass on interview speech
    # is accurate enough, and latency here is user-facing dead air.
    t0 = time.time()
    with _stt_infer_lock:
        segments, _info = model.transcribe(
            io.BytesIO(wav_bytes), beam_size=1, vad_filter=True,
            language="en", condition_on_previous_text=False,
            temperature=0.0,
        )
        text = "".join(seg.text for seg in segments).strip()
    print(f"[voice] STT transcribe: {time.time() - t0:.2f}s, {len(text)} chars", flush=True)
    return text


def _warm_stt() -> None:
    """Load + warm faster-whisper so the first real transcribe doesn't pay the
    cold start (base.en download ~140MB + model load + CUDA/CPU kernel autotune +
    silero-VAD download — the ~30s "transcribing" stall on the first question).
    Decodes a short throwaway buffer: vad=False warms the encoder/decoder kernels;
    vad=True then loads the silero VAD model real transcribes use. Raises on
    failure so the caller can report it."""
    import numpy as np
    try:
        model = _get_stt()
    except Exception:
        _state["stt_phase"] = "error"
        raise
    if _state.get("stt_warm"):
        return
    _state["stt_phase"] = "warming"
    t0 = time.time()
    warm = (np.random.randn(16000 * 2).astype(np.float32) * 0.02).clip(-1, 1)
    warm_wav = _pcm16_to_wav((warm * 32767.0).astype("<i2").tobytes(), 16000)
    with _stt_infer_lock:
        list(model.transcribe(io.BytesIO(warm_wav), beam_size=1)[0])
        list(model.transcribe(io.BytesIO(warm_wav), beam_size=1, vad_filter=True)[0])
        # The copilot's streaming path: raw float32 in, no timestamps.
        list(model.transcribe(warm, beam_size=1, language="en",
                              without_timestamps=True)[0])
        # ALSO warm on a long buffer: on CUDA the kernels are shape-tuned on first
        # use, so a warmup that only ever saw a 2s clip leaves the FIRST real long
        # question (30-60s captures are common) paying several seconds of one-time
        # autotune right when the user is waiting. ~40s of low noise covers the
        # long-shape path; vad_filter skips most of the decode so this stays cheap.
        warm_long = (np.random.randn(16000 * 40).astype(np.float32) * 0.02).clip(-1, 1)
        long_wav = _pcm16_to_wav((warm_long * 32767.0).astype("<i2").tobytes(), 16000)
        list(model.transcribe(io.BytesIO(long_wav), beam_size=1, vad_filter=True,
                              language="en", condition_on_previous_text=False)[0])
    _state["stt_warm"] = True
    _state["stt_phase"] = "ready"
    print(f"[voice] STT warmed in {time.time() - t0:.1f}s", flush=True)


def _warm_moonshine() -> None:
    """Load Moonshine and exercise its streaming path before the interview."""
    if _state.get("moonshine_warm"):
        return
    import numpy as np
    t0 = time.time()
    transcriber = _get_moonshine()
    with _moonshine_inference_lock:
        stream = transcriber.create_stream(
            update_interval=MOONSHINE_UPDATE_INTERVAL_S
        )
        stream.start()
        stream.add_audio(np.zeros(3200, dtype=np.float32), 16000)
        stream.stop()
        stream.close()
    _state["moonshine_warm"] = True
    print(f"[voice] Moonshine warmed in {time.time() - t0:.1f}s", flush=True)


def prepare(
    engine: str,
    speakers: list[str] | tuple = (),
    stt_engine: str = "whisper",
) -> dict:
    """Warm the requested speech recognizer and TTS engine (with every panel
    voice in `speakers`), blocking until ready.

    Called from POST /voice/prepare when the user starts a mock interview, behind
    the "Preparing engine…" modal, so the model-load + cold-start cost (vibe-rt's
    first synth is ~30s on a laptop GPU) lands there instead of at app startup or
    on the first question. Nothing is warmed at boot anymore — that stacked the
    STT load and the VibeVoice cold synth on the same device at launch and froze
    the app. Idempotent: a second call is near-instant once warm. Returns a
    per-stage readiness report (best-effort per stage); `engine` is the voice
    that will actually speak, `tts_fallback` the one asked for when it couldn't."""
    requested = _norm_engine(engine)
    stt_engine = "moonshine" if stt_engine == "moonshine" else "whisper"
    t0 = time.time()
    report: dict = {
        "engine": requested, "stt_engine": stt_engine, "stt": False, "tts": False
    }
    # Serialized: callers fire this fire-and-forget from several places (the
    # copilot overlay on open AND on Rec — twice each under React StrictMode —
    # plus the mock-interview modal). Concurrent warms would run two
    # transcribes on one WhisperModel, which is not thread-safe. Late callers
    # block briefly, then every stage is a warm no-op.
    with _prepare_lock:
        try:
            if stt_engine == "moonshine":
                _warm_moonshine()
            else:
                _warm_stt()
            report["stt"] = True
        except Exception as exc:
            report["stt_error"] = str(exc)
            print(f"[voice] prepare: {stt_engine} warm failed: {exc}", flush=True)
            # Keep mock interviews usable when Moonshine is missing or its model
            # download fails. This pays Whisper's warmup only on that failure.
            if stt_engine == "moonshine":
                try:
                    _warm_stt()
                    report["stt"] = True
                    report["stt_engine"] = "whisper"
                    report["stt_fallback"] = True
                except Exception as fallback_exc:
                    report["stt_fallback_error"] = str(fallback_exc)
        # Starting an interview retries an engine that failed earlier.
        _state["tts_errors"].pop(requested, None)
        # warm() is best-effort and never raises; it returns the engine that
        # will actually speak, so readiness is reported for that one.
        eng = warm(requested, speakers)
    report["engine"] = eng.name
    if eng.name != requested:
        report["tts_fallback"] = requested
        report["tts_error"] = _state["tts_errors"].get(requested, "not installed")
    report["tts"] = eng.warmed()
    report["tts_device"] = eng.device()
    report["ready"] = report["stt"] and report["tts"]
    report["took_ms"] = int((time.time() - t0) * 1000)
    print(f"[voice] prepare engine={eng.name} stt={report['stt_engine']} ready={report['ready']} "
          f"({report['took_ms']}ms)", flush=True)
    return report


def warm(engine: str | None, speakers: list[str] | tuple = ()):
    """Pay an engine's cold-start cost (model load + one throwaway line) ahead
    of the interview. Best-effort, never raises: an engine that fails is
    recorded in `tts_errors` and Piper is warmed in its place. Returns the
    engine that will speak."""
    eng = _TTS[FALLBACK_ENGINE]
    try:
        eng, _sr = _load_engine(engine, speakers)
        eng.warm(list(speakers))
    except Exception as exc:
        print(f"[voice] {eng.name} warm failed: {exc}", flush=True)
        if eng.name != FALLBACK_ENGINE:
            _state["tts_errors"][eng.name] = str(exc)
            eng = _TTS[FALLBACK_ENGINE]
            try:
                eng.warm()
            except Exception:
                pass
    return eng


# ── endpoints ────────────────────────────────────────────────────────────────
@router.get("/status")
async def status():
    """Report what's installed and loaded, and where it runs. `available` is
    the baseline (Piper + Whisper) path; `tts_engines` describes every voice
    (installed, loaded, device, panel presets, last error) and
    `default_engine` is the one an engine-less request gets. The legacy
    `vibe_available` / `voices` / `default_speaker` keys describe VibeVoice."""
    # Prefer what's already known; detecting via torch (`_device`) would block
    # the event loop on a cold torch import every time the overlay polls.
    device = _state["device"] or _state.get("stt_device") \
        or await asyncio.to_thread(_stt_device)
    detail = _state["import_error"]
    piper_ok = _TTS["piper"].available()
    stt_ok = _ASR["whisper"].available()
    available = piper_ok and stt_ok
    if not available:
        missing = [m for m, ok in
                   (("piper-tts", piper_ok), ("faster-whisper", stt_ok)) if not ok]
        detail = f"missing: {', '.join(missing)} (run backend/setup.ps1 -Voice)"
    tts_engines = {
        name: {
            "label": eng.label,
            "available": eng.available(),
            "loaded": eng.loaded(),
            "device": eng.device(),
            "voices": list(eng.voices),
            "default_speaker": eng.default_speaker,
            "error": _state["tts_errors"].get(name),
        }
        for name, eng in _TTS.items()
    }
    stt_engines = {
        name: {"available": eng.available(), "loaded": eng.loaded(), "device": eng.device()}
        for name, eng in _ASR.items()
    }
    gpu = None
    torch = sys.modules.get("torch")   # only report if something already imported it
    if torch is not None:
        try:
            if torch.cuda.is_initialized():
                gpu = {"allocated_mb": round(torch.cuda.memory_allocated() / 2**20),
                       "reserved_mb": round(torch.cuda.memory_reserved() / 2**20)}
        except Exception:
            pass
    return {
        "available": available,
        "device": device,
        "default_engine": await asyncio.to_thread(_default_engine),
        "tts_engines": tts_engines,
        "stt_engines": [n for n in ("moonshine", "whisper") if stt_engines[n]["available"]],
        "recognizers": stt_engines,
        "torch_gpu": gpu,
        "vibe_available": tts_engines["vibe-rt"]["available"],
        "moonshine_available": stt_engines["moonshine"]["available"],
        "voices": list(VIBE_VOICES.keys()),
        "default_speaker": VIBE_DEFAULT_SPEAKER,
        "tts_loaded": any(e["loaded"] for e in tts_engines.values()),
        "stt_loaded": _state["stt"] is not None,
        "stt_phase": _state["stt_phase"],
        "stt_device": _state.get("stt_device"),
        "moonshine_loaded": _state["moonshine"] is not None,
        "detail": detail,
    }


def _speakers(req: TtsRequest) -> list[str]:
    return [s for s in (req.speakers or ([req.speaker] if req.speaker else [])) if s]


@router.post("/warm")
async def warm_endpoint(req: TtsRequest):
    """Kick an engine's warmup in the background and return immediately."""
    engine = _norm_engine(req.engine)
    threading.Thread(target=warm, args=(engine, _speakers(req)), daemon=True).start()
    return {"warming": True, "engine": engine}


@router.post("/prepare")
async def prepare_endpoint(req: TtsRequest):
    """Warm STT + the chosen TTS engine and BLOCK until ready, returning a
    readiness report. The mock-interview "Preparing engine…" modal awaits this so
    the cold start happens there instead of at app startup. Runs off the event
    loop so the sidecar keeps serving other requests during the ~30s vibe-rt
    cold synth."""
    return await asyncio.to_thread(
        prepare, req.engine, _speakers(req), (req.stt_engine or "whisper").strip().lower()
    )


@router.post("/tts")
async def tts(req: TtsRequest):
    text = (req.text or "").strip()
    if not text:
        return {"error": "empty text"}

    def synth() -> tuple[bytes, int]:
        eng, sr = _load_engine(req.engine, _speakers(req))
        return _pcm16_to_wav(b"".join(eng.stream(text, req.speaker or "")), sr), sr

    try:
        wav_bytes, sr = await asyncio.to_thread(synth)
        return {"audio_b64": base64.b64encode(wav_bytes).decode("ascii"), "sample_rate": sr}
    except Exception as exc:
        return {"error": f"TTS failed: {exc}"}


@router.post("/tts_stream")
async def tts_stream(req: TtsRequest):
    """Stream synthesized PCM as it's decoded. Body = raw little-endian 16-bit
    mono PCM frames (no WAV header); the sample rate is in `X-Sample-Rate` and
    the engine that spoke (after any Piper fallback) in `X-Engine`."""
    text = (req.text or "").strip()
    if not text:
        return Response(status_code=204)
    speaker = req.speaker or ""
    # Load the model (and voice) up front, off the event loop, so the sample
    # rate is known before we commit to streaming headers, and import/download
    # errors surface as a clean 500 (or a Piper fallback) instead of a
    # half-written stream.
    try:
        eng, sr = await asyncio.to_thread(_load_engine, req.engine, [speaker] if speaker else [])
    except Exception as exc:
        return JSONResponse({"error": f"TTS unavailable: {exc}"}, status_code=500)

    def gen():
        try:
            yield from eng.stream(text, speaker)
        except Exception as exc:
            # Mid-stream failure: stop cleanly. Headers are already sent, so the
            # client just sees a short stream — better than a 500 it can't read.
            print(f"[voice] {eng.name} synth failed: {exc}", flush=True)
            return

    return StreamingResponse(
        gen(),
        media_type="application/octet-stream",
        headers={"X-Sample-Rate": str(sr), "X-Engine": eng.name},
    )


# The mock-interview mic's Moonshine stream: the recognizer contract under its
# original paths and response shapes.
@router.post("/stt-stream/start")
async def stt_stream_start():
    """Create one ordered Moonshine stream for a candidate answer."""
    try:
        session_id = await asyncio.to_thread(_asr_open, "moonshine")
        return {"session_id": session_id, "engine": "moonshine"}
    except Exception as exc:
        print(f"[voice] Moonshine stream start failed: {exc}", flush=True)
        return {"error": str(exc)}


@router.post("/stt-stream/chunk")
async def stt_stream_chunk(req: SttStreamChunkRequest):
    """Append one ordered PCM chunk and return the latest phrase state."""
    try:
        pcm = base64.b64decode(req.audio_b64)
        eng, session = _asr_get(req.session_id)
        return await asyncio.to_thread(
            eng.push, session, pcm, req.sample_rate, req.elapsed_ms
        )
    except Exception as exc:
        print(f"[voice] Moonshine stream chunk failed: {exc}", flush=True)
        return {"error": str(exc), "healthy": False}


@router.post("/stt-stream/finish")
async def stt_stream_finish(req: SttStreamSessionRequest):
    """Flush the stream and return the final answer text."""
    try:
        t0 = time.time()
        text = await asyncio.to_thread(_asr_end, req.session_id)
        print(
            f"[voice] Moonshine final: {time.time() - t0:.2f}s, "
            f"{len(text)} chars",
            flush=True,
        )
        return {"text": text, "engine": "moonshine"}
    except Exception as exc:
        print(f"[voice] Moonshine stream finish failed: {exc}", flush=True)
        return {"error": str(exc)}


@router.post("/stt-stream/cancel")
async def stt_stream_cancel(req: SttStreamSessionRequest):
    """Discard an abandoned candidate stream."""
    try:
        await asyncio.to_thread(_asr_end, req.session_id, True)
    except Exception:
        pass
    return {"cancelled": True}


@router.post("/asr/warm")
async def asr_warm():
    """Load + warm faster-whisper in the background (Rust calls this once the
    sidecar is up, so the copilot never meets a cold recognizer). Progress is
    `stt_phase` in /voice/status."""
    if not _state.get("stt_warm") and _state["stt_phase"] not in ("loading", "warming"):
        def run():
            try:
                with _prepare_lock:
                    _warm_stt()
            except Exception as exc:
                print(f"[voice] STT warm failed: {exc}", flush=True)
        threading.Thread(target=run, daemon=True).start()
    return {"stt_phase": _state["stt_phase"]}


@router.post("/asr/start")
async def asr_start(req: AsrStartRequest | None = None):
    """Open a streaming-transcription session (default Whisper: one copilot
    question)."""
    engine = (req.engine if req and req.engine else "whisper").strip().lower()
    try:
        session_id = await asyncio.to_thread(_asr_open, engine)
        return {"session_id": session_id, "engine": engine}
    except Exception as exc:
        print(f"[voice] ASR start ({engine}) failed: {exc}", flush=True)
        return {"error": str(exc)}


@router.post("/asr/chunk")
async def asr_chunk(req: AsrChunkRequest):
    """Append audio and return the live state. Whisper only schedules a
    background decode, so it runs inline; other engines decode in the call."""
    try:
        eng, session = _asr_get(req.session_id)
        pcm = base64.b64decode(req.audio_b64) if req.audio_b64 else b""
        args = (session, pcm, req.sample_rate, req.elapsed_ms, req.decode)
        if eng.name == "whisper":
            return eng.push(*args)
        return await asyncio.to_thread(eng.push, *args)
    except Exception as exc:
        return {"error": str(exc)}


@router.post("/asr/snapshot")
async def asr_snapshot(req: AsrSnapshotRequest):
    """Transcript of the question so far (blocks only if no decode covers the
    last voiced sample yet). Whisper sessions only."""
    try:
        _eng, session = _asr_get(req.session_id)
        if not hasattr(session, "snapshot"):
            raise ValueError("snapshot is Whisper-only; use /asr/finish")
        t0 = time.perf_counter()
        text, reused = await asyncio.to_thread(session.snapshot, req.upto_samples)
        return {"text": text, "reused": reused,
                "wait_ms": round((time.perf_counter() - t0) * 1000)}
    except Exception as exc:
        print(f"[voice] ASR snapshot failed: {exc}", flush=True)
        return {"error": str(exc)}


@router.post("/asr/finish")
async def asr_finish(req: SttStreamSessionRequest):
    """Final transcript of a session; closes it."""
    try:
        return {"text": await asyncio.to_thread(_asr_end, req.session_id)}
    except Exception as exc:
        return {"error": str(exc)}


@router.post("/asr/close")
async def asr_close(req: SttStreamSessionRequest):
    try:
        await asyncio.to_thread(_asr_end, req.session_id, True)
    except ValueError:
        pass
    return {"closed": True}


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


def _vad_tail(pcm: bytes) -> dict:
    """Silero-VAD verdict on a rolling audio tail: how many ms of NON-SPEECH
    trail the window. Neural speech detection — unlike the energy heuristic it
    ignores transmitted room tone, comfort noise, music and typing, so it
    neither cuts a quiet-voiced speaker short nor waits forever on a noisy
    call. Warm inference is ~4 ms for a 2 s window; the model itself is the
    one already bundled with faster-whisper (loaded during /voice/prepare)."""
    import numpy as np
    _pin_torch_cudnn()  # faster_whisper imports ctranslate2
    from faster_whisper.vad import VadOptions, get_speech_timestamps
    audio = np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768.0
    window_ms = len(audio) / 16.0
    ts = get_speech_timestamps(audio, vad_options=VadOptions(
        threshold=0.5,
        min_speech_duration_ms=100,   # ignore sub-100ms blips
        min_silence_duration_ms=150,  # merge segments split by tiny gaps
        speech_pad_ms=100,            # pad segment ends → verdict errs ~100ms safe
    ))
    if not ts:
        return {"has_speech": False, "trailing_silence_ms": window_ms,
                "window_ms": window_ms}
    trailing = max(0.0, window_ms - ts[-1]["end"] / 16.0)
    return {"has_speech": True, "trailing_silence_ms": trailing,
            "window_ms": window_ms}


_vad_stats = {"n": 0}


@router.post("/vad")
async def vad(req: VadRequest):
    """Speech/no-speech verdict for the capture loop's endpointing (see
    `_vad_tail`). Called ~4×/s with the rolling tail while a question or answer
    is being captured; must stay fast and never raise."""
    try:
        pcm = base64.b64decode(req.audio_b64)
        out = await asyncio.to_thread(_vad_tail, pcm)
        # Sampled field telemetry (~every 5s of capture at the 4/s cadence):
        # proves in sidecar.log that model endpointing is active and shows the
        # trailing-silence values the capture loop is acting on.
        _vad_stats["n"] += 1
        if _vad_stats["n"] % 20 == 1:
            print(f"[voice] vad#{_vad_stats['n']}: "
                  f"trailing={out['trailing_silence_ms']:.0f}ms "
                  f"speech={out['has_speech']}", flush=True)
        return out
    except Exception as exc:
        print(f"[voice] vad error: {exc}", flush=True)
        return {"error": str(exc)}
