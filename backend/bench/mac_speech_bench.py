"""Phase 6 step 1 — can the MacBook Pro 2017 (Intel dual-core, macOS 13) serve
mock-interview speech fast enough? Run this ON THE MAC before any server code.

Self-contained: no repo imports, no app, no API keys, no GPU. It measures
  * Piper (the interviewer voice): load, first audio + real-time factor on 20
    interviewer sentences, one sentence per call like the app
  * recognizers for your spoken answers, on 10 answers Piper speaks:
    faster-whisper tiny.en / base.en (int8) and Moonshine tiny / base (ONNX).
    "finalize" = decoding the whole answer once you stop (the worst case);
    "tail" = decoding only the last 3 s (what a streaming session leaves
    after background decodes); RTF = decode time / audio length
  * word error rate against the script

Why these engines: `moonshine-voice` (the app's Moonshine) only ships Mac
wheels for macOS 15, and onnxruntime dropped Intel Macs after 1.23.2 (as did
numba after 0.62.1 / llvmlite after 0.45.1 — newer ones try to compile).

On the Mac (Terminal), with Python 3.11 (python.org, or `uv python install 3.11`):
  python3.11 -m venv ~/interprep-bench
  source ~/interprep-bench/bin/activate
  pip install "onnxruntime==1.23.2" "numba==0.62.1" "llvmlite==0.45.1" \
      piper-tts faster-whisper useful-moonshine-onnx
  python mac_speech_bench.py            # ~10–20 min, ~600 MB of models on first run
Paste the summary it prints at the end (also saved as mac_speech_bench-*.json).
"""
from __future__ import annotations

import json
import os
import platform
import re
import statistics
import sys
import time
import urllib.request
from datetime import datetime
from pathlib import Path

import numpy as np

CACHE = Path.home() / ".cache" / "interprep-bench"
PIPER_VOICE = "en_US-amy-medium"
PIPER_URL = "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/amy/medium"
SR = 16000

LINES = [
    "Interesting.",
    "How did you measure whether that change actually worked?",
    "Let's switch gears a bit and talk about system design.",
    "Imagine you need to build a rate limiter for a public API that serves millions of requests per day.",
    "How would you approach it?",
    "What trade-offs did you consider?",
    "Okay, that makes sense.",
    "Can you give me a specific example of a time you had to deliver difficult feedback to a peer?",
    "Why this company, and why now?",
    "If you had another week on that project, what would you have changed first, and why that over everything else on your list?",
    "Got it.",
    "Walk me through how you debugged the latency regression you mentioned.",
    "Who else was involved in that decision?",
    "What was the hardest part of migrating the data without downtime?",
    "Tell me about a project you're proud of that isn't on your resume.",
    "How do you decide when a prototype is good enough to ship?",
    "Thanks, that's really helpful context.",
    "What questions do you have for me?",
    "Suppose two senior engineers on your team strongly disagree about the architecture for a new service, and the deadline is in three weeks; what do you do?",
    "Great, let's wrap up there.",
]

# Candidate-style answers, no digits (so WER isn't skewed by "eight" vs "8").
ANSWERS = [
    "In my last role I led the migration of our payments service from a monolith to three smaller services. The biggest risk was data consistency, so we ran both systems in parallel for six weeks and compared every transaction, and we had zero lost payments during the cutover.",
    "I'd start by clarifying the requirements. How many requests per second do we expect, and is the limit per user or per key? Then I'd use a token bucket in Redis, keyed by the client, with a script so the check and the decrement happen atomically.",
    "One time a teammate kept merging code without tests. Instead of calling it out in the channel, I set up a short one on one, showed two incidents that traced back to untested changes, and asked how I could help. We agreed on a lightweight checklist and the incidents dropped by half.",
    "I want to join because your team is solving real time fraud detection at a scale I haven't worked at yet. I've spent four years building streaming pipelines with Kafka and Flink, and I'm excited to apply that where latency directly affects customers.",
    "If I had another week, I would have added better observability first. We shipped without per tenant dashboards, so when one customer's traffic spiked, it took us almost an hour to find the root cause.",
    "The hardest bug I fixed was a race condition in our cache invalidation. Two workers could read a stale value between the write and the delete. I reproduced it with a stress test, then switched to versioned keys, which removed the race entirely.",
    "My approach to prioritization is to estimate impact and effort for each item, then talk to the stakeholders before committing. I'd rather ship one thing that matters this sprint than three things nobody uses.",
    "We used Postgres with read replicas, and we partitioned the events table by month. That kept most queries fast even as the table grew past two billion rows.",
    "Yes, I've used Kubernetes in production for about three years, mostly on Amazon's managed service.",
    "Honestly, I think my biggest weakness is that I sometimes over engineer early versions, so now I timebox prototypes to two days.",
]


def norm(s: str) -> list[str]:
    s = s.lower().replace("-", " ")
    return re.sub(r"[^a-z' ]+", " ", s).replace("'", "").split()


def wer(ref: str, hyp: str) -> float:
    r, h = norm(ref), norm(hyp)
    d = list(range(len(h) + 1))
    for i in range(1, len(r) + 1):
        prev, d[0] = d[0], i
        for j in range(1, len(h) + 1):
            cur = d[j]
            d[j] = min(d[j] + 1, d[j - 1] + 1, prev + (r[i - 1] != h[j - 1]))
            prev = cur
    return d[len(h)] / max(1, len(r))


def p(values, q):
    v = sorted(values)
    return v[min(len(v) - 1, int(round(q / 100 * (len(v) - 1))))]


def resample(x: np.ndarray, sr: int, to: int = SR) -> np.ndarray:
    if sr == to:
        return x.astype(np.float32)
    n = int(len(x) * to / sr)
    return np.interp(np.linspace(0, len(x), n, endpoint=False), np.arange(len(x)), x).astype(np.float32)


# ── Piper ────────────────────────────────────────────────────────────────────
def load_piper():
    try:
        from piper import PiperVoice
    except ImportError:
        from piper.voice import PiperVoice
    CACHE.mkdir(parents=True, exist_ok=True)
    model = CACHE / f"{PIPER_VOICE}.onnx"
    for suffix in ("", ".json"):
        target = Path(str(model) + suffix)
        if not target.exists():
            urllib.request.urlretrieve(f"{PIPER_URL}/{PIPER_VOICE}.onnx{suffix}", target)
    voice = PiperVoice.load(str(model))
    return voice, int(getattr(voice.config, "sample_rate", 22050))


def piper_chunks(voice, text: str):
    """int16 bytes per chunk, across piper-tts versions (same as voice.py)."""
    raw = getattr(voice, "synthesize_stream_raw", None)
    if callable(raw):
        for chunk in raw(text):
            yield bytes(chunk)
        return
    for ch in voice.synthesize(text):
        data = getattr(ch, "audio_int16_bytes", None)
        if data is None:
            arr = np.asarray(getattr(ch, "audio_float_array", ch), dtype=np.float32).flatten()
            data = (np.clip(arr, -1.0, 1.0) * 32767.0).astype("<i2").tobytes()
        yield bytes(data)


def bench_piper(voice, sr: int) -> dict:
    list(piper_chunks(voice, "Warm up."))
    firsts, rtfs = [], []
    for line in LINES:
        t0 = time.perf_counter()
        first, n = None, 0
        for chunk in piper_chunks(voice, line):
            if first is None:
                first = time.perf_counter() - t0
            n += len(chunk) // 2
        total = time.perf_counter() - t0
        firsts.append(first * 1000)
        rtfs.append(total / (n / sr))
    return {"first_ms_p50": round(p(firsts, 50)), "first_ms_p90": round(p(firsts, 90)),
            "first_ms_max": round(max(firsts)), "rtf_p50": round(p(rtfs, 50), 3)}


# ── recognizers ──────────────────────────────────────────────────────────────
def whisper_engine(name: str):
    from faster_whisper import WhisperModel
    model = WhisperModel(name, device="cpu", compute_type="int8", cpu_threads=0)

    def decode(audio: np.ndarray) -> str:
        # The app's settings (routes/voice.py `_decode_pcm`).
        segments, _ = model.transcribe(audio, beam_size=1, vad_filter=True, language="en",
                                       condition_on_previous_text=False, temperature=0.0,
                                       without_timestamps=True)
        return "".join(s.text for s in segments).strip()
    return decode


def moonshine_engine(name: str):
    from moonshine_onnx import MoonshineOnnxModel, load_tokenizer
    model = MoonshineOnnxModel(model_name=name)
    tok = load_tokenizer()

    def decode(audio: np.ndarray) -> str:
        return tok.decode_batch(model.generate(audio[np.newaxis, :].astype(np.float32)))[0].strip()
    return decode


def bench_asr(label: str, make, clips) -> dict:
    t = time.perf_counter()
    try:
        decode = make()
        decode(np.zeros(SR, dtype=np.float32) + 1e-4)   # warm
    except Exception as exc:
        print(f"  {label}: unavailable ({exc})", flush=True)
        return {"engine": label, "error": str(exc)}
    load_s = time.perf_counter() - t
    rows = []
    for text, audio in clips:
        t0 = time.perf_counter()
        hyp = decode(audio)
        full = time.perf_counter() - t0
        tail_audio = audio[-3 * SR:]
        t0 = time.perf_counter()
        decode(tail_audio)
        tail = time.perf_counter() - t0
        rows.append({"wer": wer(text, hyp), "finalize_ms": full * 1000, "tail_ms": tail * 1000,
                     "rtf": full / (len(audio) / SR), "hyp": hyp})
    out = {
        "engine": label, "load_s": round(load_s, 1),
        "wer_pct": round(100 * statistics.mean(r["wer"] for r in rows), 1),
        "finalize_ms_p50": round(p([r["finalize_ms"] for r in rows], 50)),
        "finalize_ms_max": round(max(r["finalize_ms"] for r in rows)),
        "tail_ms_p50": round(p([r["tail_ms"] for r in rows], 50)),
        "rtf_p50": round(p([r["rtf"] for r in rows], 50), 3),
        "worst": max(rows, key=lambda r: r["wer"])["hyp"],
    }
    print(f"  {label:24s} WER {out['wer_pct']:5.1f}%  finalize p50 {out['finalize_ms_p50']:6d} ms  "
          f"tail(3 s) p50 {out['tail_ms_p50']:5d} ms  RTF {out['rtf_p50']:.3f}  (load {out['load_s']}s)",
          flush=True)
    return out


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8")
    info = {"platform": platform.platform(), "machine": platform.machine(),
            "processor": platform.processor(), "cpus": os.cpu_count(),
            "python": platform.python_version()}
    try:
        import onnxruntime
        info["onnxruntime"] = onnxruntime.__version__
    except Exception:
        pass
    print("System:", info, flush=True)

    t = time.perf_counter()
    voice, psr = load_piper()
    piper = {"load_s": round(time.perf_counter() - t, 1), **bench_piper(voice, psr)}
    print(f"Piper: first audio p50 {piper['first_ms_p50']} ms, p90 {piper['first_ms_p90']} ms, "
          f"RTF {piper['rtf_p50']} (load {piper['load_s']}s)", flush=True)

    clips = []
    for text in ANSWERS:
        pcm = np.frombuffer(b"".join(piper_chunks(voice, text)), dtype="<i2").astype(np.float32) / 32768.0
        clips.append((text, np.concatenate([np.zeros(SR // 4, np.float32), resample(pcm, psr),
                                            np.zeros(SR // 2, np.float32)])))
    secs = sum(len(a) for _, a in clips) / SR
    print(f"Recognizers on {len(clips)} answers ({secs:.0f} s of speech):", flush=True)
    asr = [
        bench_asr("whisper tiny.en int8", lambda: whisper_engine("tiny.en"), clips),
        bench_asr("whisper base.en int8", lambda: whisper_engine("base.en"), clips),
        bench_asr("moonshine tiny (onnx)", lambda: moonshine_engine("moonshine/tiny"), clips),
        bench_asr("moonshine base (onnx)", lambda: moonshine_engine("moonshine/base"), clips),
    ]

    result = {"stamp": datetime.now().isoformat(timespec="seconds"), "system": info,
              "piper": piper, "asr": asr}
    path = Path(f"mac_speech_bench-{datetime.now():%Y%m%d-%H%M%S}.json")
    path.write_text(json.dumps(result, indent=2), encoding="utf-8")
    print("\n=== paste this back ===")
    print(json.dumps({"system": info, "piper": piper,
                      "asr": [{k: v for k, v in a.items() if k != "worst"} for a in asr]}, indent=1))
    print(f"(full results: {path.resolve()})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
