"""Phase 5 gate — interviewer voice engines (see the InterPrep Copilot Upgrade
Blueprint).

Spawns its own sidecar (no API spend) and measures, per TTS engine:
  * prepare      POST /voice/prepare from cold: model load + warm-up, i.e. what
                 the mock interview's "Preparing engine…" modal waits for
  * first audio  POST /voice/tts_stream → first PCM bytes, over HTTP exactly
                 like the Rust voice worker, one sentence per request (the app
                 streams sentence by sentence), on 20 interviewer lines the
                 warm-up never saw; a panel rotates through the engine's voices
  * RTF          total synth time / audio seconds
  * GPU memory   nvidia-smi "used" before the sidecar, after the copilot's
                 Whisper loads (/voice/asr/warm, as the app does at startup)
                 and after each voice, so the sidecar's footprint next to a
                 meeting app is known

Gate: Kokoro's p50 first audio ≤ Piper's, and the GPU keeps ≥ 1 GB free for
the meeting app with Whisper + Kokoro resident.

  cd backend
  .\\.venv\\Scripts\\python.exe bench\\tts_gate.py            # kokoro vs piper
  .\\.venv\\Scripts\\python.exe bench\\tts_gate.py --vibe     # + VibeVoice (slow cold start)
"""
from __future__ import annotations

import argparse
import json
import secrets
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

import httpx

sys.path.insert(0, str(Path(__file__).resolve().parent))
import latency_harness as lh  # noqa: E402

# One sentence each, as App.tsx hands them to voice_speak_chunk: short
# acknowledgements, typical questions, and long multi-clause ones.
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


def nvidia_smi(field: str) -> int | None:
    """One GPU-wide memory figure in MiB ("memory.used" / "memory.total")."""
    try:
        out = subprocess.check_output(
            ["nvidia-smi", f"--query-gpu={field}", "--format=csv,noheader,nounits"],
            text=True, timeout=10)
        return int(out.strip().splitlines()[0])
    except Exception:
        return None


def gpu_used_mb() -> int | None:
    return nvidia_smi("memory.used")


def speak(client: httpx.Client, base: str, engine: str, speaker: str, text: str) -> dict:
    """One /voice/tts_stream call: ms to the first PCM bytes and to the end."""
    t0 = time.perf_counter()
    first = None
    n = 0
    with client.stream("POST", f"{base}/voice/tts_stream", timeout=120,
                       json={"text": text, "engine": engine, "speaker": speaker}) as r:
        r.raise_for_status()
        sr = int(r.headers.get("x-sample-rate", "0"))
        used = r.headers.get("x-engine", "?")
        for chunk in r.iter_raw():
            if chunk and first is None:
                first = time.perf_counter() - t0
            n += len(chunk)
    total = time.perf_counter() - t0
    audio_s = (n // 2) / sr if sr else 0.0
    return {"text": text, "speaker": speaker, "engine_used": used,
            "first_ms": round(first * 1000) if first is not None else None,
            "total_ms": round(total * 1000), "audio_s": round(audio_s, 2),
            "rtf": round(total / audio_s, 3) if audio_s else None}


def run_engine(client, base: str, engine: str, panel: int) -> dict:
    status = client.get(f"{base}/voice/status", timeout=60).json()
    info = status["tts_engines"][engine]
    if not info["available"]:
        print(f"  {engine}: not installed — skipped")
        return {"engine": engine, "skipped": "not installed"}
    voices = info["voices"][:panel] if info["voices"] else [""]
    t0 = time.perf_counter()
    rep = client.post(f"{base}/voice/prepare", timeout=900, json={
        "text": "", "engine": engine, "speaker": voices[0], "speakers": voices,
        "stt_engine": "moonshine"}).json()
    prepare_s = time.perf_counter() - t0
    print(f"  {engine}: prepare {prepare_s:.1f}s → engine={rep.get('engine')} "
          f"tts={rep.get('tts')} device={rep.get('tts_device')}"
          + (f"  FELL BACK: {rep.get('tts_error')}" if rep.get("tts_fallback") else ""))
    gpu = gpu_used_mb()
    rows = []
    for i, line in enumerate(LINES):
        row = speak(client, base, engine, voices[i % len(voices)], line)
        rows.append(row)
        print(f"    first {row['first_ms']:>5} ms  total {row['total_ms']:>5} ms  "
              f"audio {row['audio_s']:>5.2f}s  [{row['speaker'] or '-'}] {line[:52]}")
    firsts = [r["first_ms"] for r in rows]
    rtfs = [r["rtf"] for r in rows if r["rtf"] is not None]
    summary = {"engine": engine, "engine_used": rep.get("engine"), "device": rep.get("tts_device"),
               "voices": voices, "prepare_s": round(prepare_s, 1), "gpu_used_mb": gpu,
               "first_ms_p50": lh.pct(firsts, 50), "first_ms_p90": lh.pct(firsts, 90),
               "first_ms_max": max(firsts), "rtf_p50": lh.pct(rtfs, 50), "rows": rows}
    print(f"  {engine}: first audio p50 {summary['first_ms_p50']} ms, p90 {summary['first_ms_p90']} ms, "
          f"max {summary['first_ms_max']} ms; RTF p50 {summary['rtf_p50']}; GPU used {gpu} MiB")
    return summary


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--vibe", action="store_true", help="also measure VibeVoice")
    ap.add_argument("--panel", type=int, default=4, help="panel voices to rotate (default 4)")
    ap.add_argument("--no-whisper", action="store_true", help="skip loading the copilot's Whisper first")
    args = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")

    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    lh.RESULTS.mkdir(parents=True, exist_ok=True)
    log_path = lh.RESULTS / f"{stamp}-tts-gate-sidecar.log"
    total = nvidia_smi("memory.total")
    base_gpu = gpu_used_mb()
    print(f"GPU: {base_gpu} / {total} MiB used before the sidecar")
    proc, base, boot_s = lh.start_sidecar(secrets.token_hex(16), log_path)
    out: dict = {"stamp": stamp, "gpu_total_mb": total, "gpu_before_mb": base_gpu,
                 "boot_s": round(boot_s, 1), "engines": []}
    try:
        with httpx.Client() as client:
            if not args.no_whisper:
                t0 = time.perf_counter()
                client.post(f"{base}/voice/asr/warm", timeout=30)
                while client.get(f"{base}/voice/status", timeout=60).json()["stt_phase"] not in ("ready", "error"):
                    time.sleep(1)
                out["whisper_warm_s"] = round(time.perf_counter() - t0, 1)
                out["gpu_whisper_mb"] = gpu_used_mb()
                print(f"Whisper (copilot) warm in {out['whisper_warm_s']}s; GPU used {out['gpu_whisper_mb']} MiB")
            engines = ["piper", "kokoro"] + (["vibe-rt"] if args.vibe else [])
            for engine in engines:
                out["engines"].append(run_engine(client, base, engine, args.panel))
            out["status"] = client.get(f"{base}/voice/status", timeout=60).json()
    finally:
        lh.stop_sidecar(proc)

    by = {e["engine"]: e for e in out["engines"]}
    k, p = by.get("kokoro", {}), by.get("piper", {})
    speed_ok = (k.get("first_ms_p50") is not None and p.get("first_ms_p50") is not None
                and k["first_ms_p50"] <= p["first_ms_p50"])
    # Memory gate on Whisper + Piper + Kokoro resident (read right after
    # Kokoro's run, before any --vibe run piles VibeVoice on top).
    peak = k.get("gpu_used_mb") or out.get("gpu_whisper_mb") or 0
    out["gpu_peak_mb"] = peak
    out["gpu_sidecar_mb"] = peak - (base_gpu or 0)
    free = (total - peak) if total else None
    mem_ok = free is not None and free >= 1024
    out["gate"] = {"kokoro_first_p50_le_piper": speed_ok, "gpu_free_mb": free,
                   "gpu_free_ge_1gb": mem_ok, "pass": speed_ok and mem_ok}
    path = lh.RESULTS / f"{stamp}-tts-gate.json"
    path.write_text(json.dumps(out, indent=2), encoding="utf-8")
    print()
    print(f"Kokoro first audio p50 {k.get('first_ms_p50')} ms vs Piper {p.get('first_ms_p50')} ms → "
          f"{'PASS' if speed_ok else 'FAIL'}")
    print(f"GPU: sidecar adds {out['gpu_sidecar_mb']} MiB (peak {peak}/{total}); "
          f"{free} MiB free for the meeting app → {'PASS' if mem_ok else 'FAIL'}")
    print(f"Results: {path}")
    return 0 if out["gate"]["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
