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
import time
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


class VadRequest(BaseModel):
    # Base64-encoded RAW little-endian 16-bit mono PCM at 16 kHz (no WAV header)
    # — the rolling audio tail the capture loop wants a speech/no-speech verdict
    # for. Raw PCM keeps the 4×/s hot path allocation-light on both sides.
    audio_b64: str


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
            # Only ask onnxruntime for CUDA when its CUDA provider is actually
            # installed (the plain `onnxruntime` wheel is CPU-only). Requesting
            # a missing provider sent ORT through a ~80s probe-and-fallback at
            # load (observed in the field, blocking the "Preparing engine…"
            # modal) — and Piper is faster than real-time on CPU regardless.
            use_cuda = False
            try:
                import onnxruntime
                use_cuda = (_device() == "cuda"
                            and "CUDAExecutionProvider" in onnxruntime.get_available_providers())
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


def _pin_torch_cudnn() -> None:
    """Preload torch's bundled cuDNN DLLs before ctranslate2 can load its own.

    ctranslate2 (faster-whisper's backend) ships a LONE cudnn64_9.dll. If STT
    loads first — the normal prepare() order — Windows registers that copy,
    and when VibeVoice's torch stack later asks cuDNN for symbols the lone
    DLL can't serve without its companion DLLs, the process HARD-CRASHES
    mid-synth ("Could not load symbol cudnnGetLibConfig. Error code 127" —
    kills the whole sidecar, observed 2026-07-07). Loading torch's complete
    cuDNN set first makes the loader dedupe by module name so both libraries
    share the good copies. Best-effort no-op when torch isn't installed."""
    try:
        import ctypes
        import glob as _glob
        import os as _os
        import torch
        lib = _os.path.join(_os.path.dirname(torch.__file__), "lib")
        for dll in sorted(_glob.glob(_os.path.join(lib, "cudnn*.dll"))):
            try:
                ctypes.WinDLL(dll)
            except OSError:
                pass
    except Exception:
        pass


def _get_stt():
    """Load faster-whisper once, on the auto-detected device."""
    if _state["stt"] is not None:
        return _state["stt"]
    with _lock:
        if _state["stt"] is None:
            import os
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
                device = _device()  # auto: cuda when available
            if device == "cuda" and _device() != "cuda":
                device = "cpu"
            compute = "float16" if device == "cuda" else "int8"
            # English-only "base.en" — faster AND more accurate than multilingual
            # "base" for English interviews. Override with INTERPREP_STT_MODEL
            # (e.g. "tiny.en" for max speed, "small.en" if transcripts are weak).
            model_name = os.environ.get("INTERPREP_STT_MODEL", "base.en")
            t0 = time.time()
            _state["stt"] = WhisperModel(
                model_name, device=device, compute_type=compute, cpu_threads=0,
            )
            print(f"[voice] STT loaded: model={model_name} device={device} "
                  f"({time.time() - t0:.1f}s)", flush=True)
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
    model = _get_stt()
    if _state.get("stt_warm"):
        return
    t0 = time.time()
    warm = (np.random.randn(16000 * 2).astype(np.float32) * 0.02).clip(-1, 1)
    warm_wav = _pcm16_to_wav((warm * 32767.0).astype("<i2").tobytes(), 16000)
    list(model.transcribe(io.BytesIO(warm_wav), beam_size=1)[0])
    list(model.transcribe(io.BytesIO(warm_wav), beam_size=1, vad_filter=True)[0])
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
    print(f"[voice] STT warmed in {time.time() - t0:.1f}s", flush=True)


def prepare(engine: str, speaker: str = VIBE_DEFAULT_SPEAKER) -> dict:
    """Warm everything a live interview needs — speech recognition (faster-whisper)
    + the chosen TTS engine — and BLOCK until ready.

    Called from POST /voice/prepare when the user starts a mock interview, behind
    the "Preparing engine…" modal, so the model-load + cold-start cost (vibe-rt's
    first synth is ~30s on a laptop GPU) lands there instead of at app startup or
    on the first question. Nothing is warmed at boot anymore — that stacked the
    STT load and the VibeVoice cold synth on the same device at launch and froze
    the app. Idempotent: a second call is near-instant once warm. Returns a
    per-stage readiness report (best-effort per stage)."""
    engine = _norm_engine(engine)
    speaker = _norm_speaker(speaker)
    t0 = time.time()
    report: dict = {"engine": engine, "stt": False, "tts": False}
    # Serialized: callers fire this fire-and-forget from several places (the
    # copilot overlay on open AND on Rec — twice each under React StrictMode —
    # plus the mock-interview modal). Concurrent warms would run two
    # transcribes on one WhisperModel, which is not thread-safe. Late callers
    # block briefly, then every stage is a warm no-op.
    with _prepare_lock:
        try:
            _warm_stt()
            report["stt"] = True
        except Exception as exc:
            report["stt_error"] = str(exc)
            print(f"[voice] prepare: STT warm failed: {exc}", flush=True)
        # warm() is best-effort and never raises, so read the loaded-state
        # directly to report TTS readiness honestly.
        warm(engine, speaker)
    report["tts"] = bool(_state["vibe_warm"]) if engine == "vibe-rt" \
        else (_state["piper"] is not None)
    report["ready"] = report["stt"] and report["tts"]
    report["took_ms"] = int((time.time() - t0) * 1000)
    print(f"[voice] prepare engine={engine} ready={report['ready']} "
          f"({report['took_ms']}ms)", flush=True)
    return report


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


@router.post("/prepare")
async def prepare_endpoint(req: TtsRequest):
    """Warm STT + the chosen TTS engine and BLOCK until ready, returning a
    readiness report. The mock-interview "Preparing engine…" modal awaits this so
    the cold start happens there instead of at app startup. Runs off the event
    loop so the sidecar keeps serving other requests during the ~30s vibe-rt
    cold synth."""
    engine = _norm_engine(req.engine)
    speaker = _norm_speaker(req.speaker)
    return await asyncio.to_thread(prepare, engine, speaker)


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


def _vad_tail(pcm: bytes) -> dict:
    """Silero-VAD verdict on a rolling audio tail: how many ms of NON-SPEECH
    trail the window. Neural speech detection — unlike the energy heuristic it
    ignores transmitted room tone, comfort noise, music and typing, so it
    neither cuts a quiet-voiced speaker short nor waits forever on a noisy
    call. Warm inference is ~4 ms for a 2 s window; the model itself is the
    one already bundled with faster-whisper (loaded during /voice/prepare)."""
    import numpy as np
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
