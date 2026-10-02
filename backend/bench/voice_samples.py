"""Listen-test the interviewer voices: the same interviewer lines spoken by
every engine and panel voice, written as WAVs to bench/results/voice-samples/.

It also loads Kokoro and VibeVoice at the same moment on two threads first —
the race that once left Kokoro with empty (meta) weights and Piper speaking in
its place — and reports whether each loaded.

  cd backend
  .\\.venv\\Scripts\\python.exe bench\\voice_samples.py            # all engines
  .\\.venv\\Scripts\\python.exe bench\\voice_samples.py --no-vibe  # skip VibeVoice
"""
from __future__ import annotations

import argparse
import sys
import threading
import time
from pathlib import Path

BACKEND = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(BACKEND))

# One reply as the app speaks it: sentence by sentence, joined back to back.
REPLY = [
    "Thanks for walking me through that.",
    "I'd like to dig into the migration you mentioned.",
    "What was the hardest trade-off you had to make, and how did you decide?",
    "Take your time, there's no rush.",
]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--no-vibe", action="store_true", help="skip VibeVoice (slow cold start)")
    args = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    from routes import voice

    out = Path(__file__).resolve().parent / "results" / "voice-samples"
    out.mkdir(parents=True, exist_ok=True)

    # VibeVoice first: its long from_pretrained is the window Kokoro must land in.
    engines = ([] if args.no_vibe else ["vibe-rt"]) + ["kokoro"]
    errors: dict[str, str] = {}

    def load(name: str) -> None:
        try:
            eng = voice._TTS[name]
            eng.load([eng.default_speaker])
        except Exception as exc:  # recorded and reported below
            errors[name] = str(exc)

    t0 = time.time()
    threads = [threading.Thread(target=load, args=(n,)) for n in engines]
    for th in threads:
        th.start()
        time.sleep(1.0)
    for th in threads:
        th.join()
    for name in engines:
        print(f"concurrent load {name}: {'FAILED ' + errors[name] if name in errors else 'ok'}"
              f"  ({time.time() - t0:.0f}s)", flush=True)

    plan = [("piper", "")]
    plan += [("kokoro", s) for s in ("Heart", "Bella", "Michael", "Fenrir")]
    if not args.no_vibe:
        plan += [("vibe-rt", s) for s in ("Emma", "Carter", "Grace", "Davis")]
    for name, speaker in plan:
        if name in errors:
            continue
        eng, sr = voice._load_engine(name, [speaker] if speaker else [])
        if eng.name != name:
            print(f"{name}: fell back to {eng.name} — skipped", flush=True)
            continue
        eng.warm([speaker] if speaker else [])
        t = time.perf_counter()
        pcm = b"".join(b"".join(eng.stream(line, speaker)) for line in REPLY)
        path = out / f"{name}-{(speaker or 'amy').lower()}.wav"
        path.write_bytes(voice._pcm16_to_wav(pcm, sr))
        print(f"{path.name}: {len(pcm) / 2 / sr:.1f}s audio in {time.perf_counter() - t:.1f}s", flush=True)
    print(f"Samples: {out}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
