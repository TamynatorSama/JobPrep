"""Phase 4 gate: screenshot → answer (see the InterPrep Copilot Upgrade
Blueprint). Spawns a sidecar, then runs the Rust example `screen_probe` N
times. Each run opens a coding problem with a cloaked magenta "overlay" on top,
captures it through the app's real path (GDI grab, Windows OCR, JPEG), checks
the overlay stayed out of the capture, and streams the answer in "screen"
mode. Gate: overlay absent from every capture; p50 keypress → first answer
token ≤ 2,000 ms.

  cd backend
  .\\.venv\\Scripts\\python.exe bench\\screen_gate.py --provider gemini --runs 5
  .\\.venv\\Scripts\\python.exe bench\\screen_gate.py --no-build   # reuse the last probe build

Spends real tokens (one coding answer per run, ~1.5k input incl. the image).
"""
from __future__ import annotations

import argparse
import json
import secrets
import statistics
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import latency_harness as lh  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--provider", help="gemini | openai | anthropic (default: your Settings choice)")
    ap.add_argument("--model", help="pin one model")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--job", default="")
    ap.add_argument("--no-build", action="store_true", help="run the last-built screen_probe example")
    args = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")

    tauri = lh.BACKEND.parent / "src-tauri"
    exe = tauri / "target" / "debug" / "examples" / "screen_probe.exe"
    if not args.no_build:
        print("building the screen_probe example (cargo)…")
        subprocess.run(["cargo", "build", "-q", "--example", "screen_probe"], cwd=tauri, check=True)
    cfg = lh.llm_config(args)
    _, context = lh.job_context(args.job)
    token = secrets.token_hex(16)
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    lh.RESULTS.mkdir(parents=True, exist_ok=True)
    log = lh.RESULTS / f"{stamp}-screen-sidecar.log"
    proc, base, boot = lh.start_sidecar(token, log)
    print(f"sidecar ready in {boot:.1f}s; provider={cfg['provider']}")
    rows = []
    try:
        import httpx
        httpx.post(f"{base}/config/seed", json={"llm": cfg}, headers={"X-InterPrep-Token": token}, timeout=120)
        t0 = time.time()
        while time.time() - t0 < 120 and "LLM channel warm" not in log.read_text(encoding="utf-8", errors="replace"):
            time.sleep(0.25)
        conf = json.dumps({"url": base, "llm": cfg, "job_context": context})
        for i in range(args.runs):
            out = subprocess.run([str(exe)], input=conf, capture_output=True, text=True, encoding="utf-8", cwd=tauri)
            try:
                row = json.loads(out.stdout.strip().splitlines()[-1])
            except (IndexError, json.JSONDecodeError):
                print(f"[{i + 1}] probe failed: {out.stderr[-400:]}")
                continue
            rows.append(row)
            print(f"[{i + 1}/{args.runs}] overlay_excluded={row['overlay_excluded']} "
                  f"(control {row['control_magenta_px']} px) capture={row['capture_ms']}ms "
                  f"first_token={row.get('request_to_first_token_ms')}ms "
                  f"keypress→first={row.get('keypress_to_first_token_ms')}ms ocr={row['ocr_keywords']} "
                  f"{row.get('error') or ''}")
    finally:
        lh.stop_sidecar(proc)

    if not rows:
        return 1
    firsts = [r["keypress_to_first_token_ms"] for r in rows if r.get("keypress_to_first_token_ms")]
    p50 = statistics.median(firsts) if firsts else None
    excluded = all(r["overlay_excluded"] and r["control_magenta_px"] > 0 for r in rows)
    print(f"\ncapture p50 {statistics.median(r['capture_ms'] for r in rows):.0f} ms · "
          f"keypress → first token p50 {p50} ms, max {max(firsts) if firsts else None} ms")
    print(f"Phase 4 gate: overlay absent {'PASS' if excluded else 'FAIL'}; "
          f"first word ≤ 2 s {'PASS' if p50 is not None and p50 <= 2000 else 'FAIL'}")
    print("\nlast answer:\n" + rows[-1].get("answer", ""))
    out_path = lh.RESULTS / f"{stamp}-screen.json"
    out_path.write_text(json.dumps({"provider": cfg["provider"], "rows": rows}, indent=2), encoding="utf-8")
    print(f"\nsaved {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
