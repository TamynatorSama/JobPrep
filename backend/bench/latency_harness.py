"""Copilot latency + token harness — the Phase 0 baseline every later change is
measured against (see the InterPrep Copilot Upgrade Blueprint).

What it measures, per interview question:
  * sidecar boot      — spawn → /health answers
  * STT               — Piper speaks the question, /voice/stt transcribes it:
                        decode time + word error rate vs the original text
  * copilot answer    — /chat/stream in "copilot" mode with your real job
                        context: time-to-first-token, total time, answer words
  * tokens            — input / cached / output tokens per answer, from the
                        sidecar's token meter (/metrics/usage)

Not measured here: end-of-question detection, which lives in the Rust capture
loop (voice_audio.rs) — its constants are 650-1,100 ms after the speaker stops.
STT runs on clean synthesized speech, so real meeting audio will be somewhat
slower and less accurate; use it to compare engines, not as an absolute.

It spawns its own sidecar on a free port, reads your LLM provider + keys from
Windows Credential Manager (the same entries the app uses; env vars
INTERPREP_LLM_PROVIDER / INTERPREP_GEMINI_KEY / INTERPREP_OPENAI_KEY /
INTERPREP_ANTHROPIC_KEY override them), and builds the job context from
%LOCALAPPDATA%\\InterPrep\\jobs.json exactly like the copilot overlay does.
Keys and job text are never printed or saved; results go to bench/results/.

LLM runs spend real tokens on your account (20 short copilot answers).

  cd backend
  .\\.venv\\Scripts\\python.exe bench\\latency_harness.py                  # full run
  .\\.venv\\Scripts\\python.exe bench\\latency_harness.py --model gemini-2.5-flash
  .\\.venv\\Scripts\\python.exe bench\\latency_harness.py --skip-llm       # speech only, no API spend
  .\\.venv\\Scripts\\python.exe bench\\latency_harness.py --questions 5 --job stripe

--pipeline (Phase 2 gate): replays each question as audio through the copilot's
real live-listening pipeline (Rust example `copilot_replay`: Silero VAD,
endpointing, streaming Whisper session, speculative answer start) in real time
and measures interviewer-stops → first answer token. Adds four two-part
questions with a 0.7 s think-pause to exercise the speculative restart, and
transcribes the same audio through the old batch /voice/stt for the accuracy
comparison. Needs cargo (builds the example on first run).
"""
from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import socket
import statistics
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

import httpx

BACKEND = Path(__file__).resolve().parent.parent
RESULTS = Path(__file__).resolve().parent / "results"

QUESTIONS = [
    "Tell me about yourself.",
    "Why do you want to work here?",
    "Tell me about a time you disagreed with a teammate and how you resolved it.",
    "Describe a project you're most proud of.",
    "What's your biggest weakness?",
    "Tell me about a time you failed.",
    "How would you design a rate limiter for a public API?",
    "Walk me through how you'd debug a sudden spike in p99 latency.",
    "How do you prioritize when everything is urgent?",
    "Tell me about a time you had to learn something quickly.",
    "What would you do in your first ninety days in this role?",
    "How do you handle feedback you disagree with?",
    "Explain a complex technical concept to a non-technical stakeholder.",
    "Tell me about a time you led without formal authority.",
    "How would you scale a service that suddenly gets ten times the traffic?",
    "What's a decision you made with incomplete data?",
    "Where do you see yourself in five years?",
    "Describe a time you improved a process.",
    "How do you make sure the code you ship is reliable?",
    "Do you have any questions for us?",
]


# Question, think-pause, follow-up: the interviewer keeps talking after a pause
# long enough to start a speculative answer, which must be cancelled.
TWO_PART = [
    ("Tell me about a project you led.", "What would you do differently next time?"),
    ("Walk me through your current role.", "Which part of it do you enjoy the most?"),
    ("Let's talk about system design.", "How would you build a URL shortener?"),
    ("You mentioned a migration on your resume.", "How did you keep the old system running during it?"),
]
THINK_PAUSE_S = 0.7
LEAD_S, TAIL_S = 0.8, 3.0  # silence around each replayed question (tail > commit)


# ── setup ────────────────────────────────────────────────────────────────────
def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def llm_config(args) -> dict:
    """Provider + keys from Credential Manager (service "InterPrep"), env wins."""
    def cred(field: str) -> str:
        # The app's Rust `keyring` crate stores generic credentials under the
        # target "<field>.InterPrep" with a UTF-16 blob — a layout Python's
        # keyring package doesn't look up, so read it directly.
        try:
            from win32ctypes.pywin32 import win32cred
            c = win32cred.CredRead(f"{field}.InterPrep", win32cred.CRED_TYPE_GENERIC)
            blob = c["CredentialBlob"]
            return blob.decode("utf-16-le") if isinstance(blob, bytes) else str(blob)
        except Exception:
            return ""

    cfg = {
        "provider": os.environ.get("INTERPREP_LLM_PROVIDER") or cred("llm_provider") or "gemini",
        "gemini_api_key": os.environ.get("INTERPREP_GEMINI_KEY") or cred("gemini_api_key"),
        "openai_api_key": os.environ.get("INTERPREP_OPENAI_KEY") or cred("openai_api_key"),
        "anthropic_api_key": os.environ.get("INTERPREP_ANTHROPIC_KEY") or cred("anthropic_api_key"),
        "model": args.model or "",
    }
    if args.provider:
        cfg["provider"] = args.provider
    return cfg


def job_context(selector: str) -> tuple[str, str]:
    """(label, context) built the same way App.tsx mirrors it to the overlay:
    JD 1.5k + research dossier 6k + resume 8k chars (App.tsx fullJobContext)."""
    base = Path(os.environ.get("LOCALAPPDATA", ".")) / "InterPrep"
    try:
        jobs = json.loads((base / "jobs.json").read_text(encoding="utf-8"))
    except Exception:
        return "", ""
    try:
        resumes = json.loads((base / "resumes.json").read_text(encoding="utf-8"))
    except Exception:
        resumes = []
    live = [j for j in jobs if not j.get("archived")]
    if selector.isdigit():
        pick = live[int(selector)] if int(selector) < len(live) else None
    elif selector:
        pick = next((j for j in live if selector.lower() in
                     f"{j.get('company', '')} {j.get('role', '')}".lower()), None)
    else:
        pick = next((j for j in live if (j.get("jobDescription") or "").strip()), live[0] if live else None)
    if not pick:
        return "", ""
    research = next((c for c in pick.get("chats", [])
                     if c.get("id") == f"c-research-{pick.get('id')}" or c.get("title") == "Company Research"), None)
    research_text = "\n\n".join(
        m.get("content", "") for m in (research or {}).get("messages", [])
        if m.get("role") == "ai" and not m.get("streaming") and m.get("content", "").strip()
    )
    resume = (pick.get("tailoredResume") or "").strip() or (resumes[-1].get("text", "").strip() if resumes else "")
    parts = [
        f"Company: {pick.get('company', '')}",
        f"Role: {pick.get('role', '')}",
        f"Location: {pick['location']}" if pick.get("location") else "",
        f"\nJob Description:\n{pick['jobDescription'][:1500]}" if pick.get("jobDescription") else "",
        f"\nCompany Research Dossier:\n{research_text[:6000]}" if research_text else "",
        f"\nCandidate Resume:\n{resume[:8000]}" if resume else "",
    ]
    return f"{pick.get('role', '')} · {pick.get('company', '')}", "\n".join(p for p in parts if p)


def start_sidecar(token: str, log_path: Path) -> tuple[subprocess.Popen, str, float]:
    port = free_port()
    # PYTHONIOENCODING: like the app (sidecar.rs) — stdout is a file, not cp1252-safe.
    env = {**os.environ, "INTERPREP_BRIDGE_TOKEN": token, "PYTHONIOENCODING": "utf-8"}
    log = open(log_path, "w", encoding="utf-8")
    t0 = time.perf_counter()
    proc = subprocess.Popen(
        [sys.executable, "-u", "-m", "uvicorn", "main:app", "--host", "127.0.0.1",
         "--port", str(port), "--log-level", "warning", "--no-access-log"],
        cwd=BACKEND, env=env, stdout=log, stderr=subprocess.STDOUT,
    )
    base = f"http://127.0.0.1:{port}"
    deadline = time.time() + 180
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"sidecar exited early — see {log_path}")
        try:
            if httpx.get(f"{base}/health", timeout=1).status_code == 200:
                return proc, base, time.perf_counter() - t0
        except httpx.HTTPError:
            pass
        time.sleep(0.1)
    stop_sidecar(proc)
    raise RuntimeError(f"sidecar not ready in 180s — see {log_path}")


def stop_sidecar(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        # Same as the app's shutdown: kill the whole tree on Windows.
        subprocess.run(["taskkill", "/T", "/F", "/PID", str(proc.pid)],
                       capture_output=True, check=False)
        proc.kill()


# ── measurements ─────────────────────────────────────────────────────────────
def words(s: str) -> list[str]:
    return re.sub(r"[^a-z0-9' ]+", " ", s.lower()).split()


def wer(ref: str, hyp: str) -> float:
    r, h = words(ref), words(hyp)
    if not r:
        return 0.0
    d = list(range(len(h) + 1))
    for i, rw in enumerate(r, 1):
        prev, d[0] = d[0], i
        for j, hw in enumerate(h, 1):
            cur = min(d[j] + 1, d[j - 1] + 1, prev + (rw != hw))
            prev, d[j] = d[j], cur
    return d[len(h)] / len(r)


def usage(client: httpx.Client, base: str, token: str) -> dict:
    rows = client.get(f"{base}/metrics/usage", headers={"X-InterPrep-Token": token}).json()["rows"]
    return {(r["feature"], r["model"]): r for r in rows}


def usage_delta(before: dict, after: dict, feature: str) -> dict:
    out = {"model": "", "input_tokens": 0, "cached_input_tokens": 0, "output_tokens": 0}
    for key, row in after.items():
        if key[0] != feature:
            continue
        prev = before.get(key, {})
        if row["calls"] - prev.get("calls", 0) > 0:
            out["model"] = key[1]
            for k in ("input_tokens", "cached_input_tokens", "output_tokens"):
                out[k] += row[k] - prev.get(k, 0)
    return out


def stream_answer(client: httpx.Client, base: str, body: dict) -> dict:
    t0 = time.perf_counter()
    ttft, text, error = None, [], ""
    with client.stream("POST", f"{base}/chat/stream", json=body, timeout=120) as resp:
        for line in resp.iter_lines():
            if not line.startswith("data:"):
                continue
            ev = json.loads(line[5:].strip() or "{}")
            kind = ev.get("type")
            if kind == "token":
                if ttft is None:
                    ttft = time.perf_counter() - t0
                text.append(ev.get("content", ""))
            elif kind == "error":
                error = str(ev.get("content", ""))[:200]
                break
            elif kind == "done":
                break
    answer = "".join(text)
    return {"ttft_ms": None if ttft is None else round(ttft * 1000),
            "total_ms": round((time.perf_counter() - t0) * 1000),
            "answer_words": len(answer.split()), "error": error, "answer": answer}


def wav_to_pcm(b64: str):
    """(int16 numpy samples, sample rate) from a base64 mono WAV."""
    import base64
    import io
    import wave

    import numpy as np
    with wave.open(io.BytesIO(base64.b64decode(b64))) as w:
        return np.frombuffer(w.readframes(w.getnframes()), dtype="<i2").copy(), w.getframerate()


def speech_end(pcm, sr: int) -> float:
    """Seconds to the end of the last 10 ms window louder than -40 dBFS."""
    import numpy as np
    win = sr // 100
    n = len(pcm) // win
    frames = (pcm[: n * win].astype(np.float64) / 32768.0).reshape(n, win)
    rms = np.sqrt((frames ** 2).mean(axis=1))
    loud = np.nonzero(rms > 0.01)[0]
    return (loud[-1] + 1) * win / sr if loud.size else 0.0


def run_pipeline(args, client, base: str, cfg: dict, context: str, stamp: str) -> list[dict]:
    """Replay synthesized questions through the Rust live-listening pipeline."""
    import base64
    import wave

    import numpy as np
    work = RESULTS / f"{stamp}-replay"
    work.mkdir(parents=True, exist_ok=True)
    plan = [(q, None) for q in QUESTIONS[: max(1, min(args.questions, len(QUESTIONS)))]] + list(TWO_PART)
    items, batch_wer = [], {}
    for i, (first, second) in enumerate(plan):
        parts = []
        for text in (first, second):
            if text:
                tts = client.post(f"{base}/voice/tts", json={"text": text, "engine": "piper"}).json()
                pcm, sr = wav_to_pcm(tts["audio_b64"])
                parts.append(pcm[: int(speech_end(pcm, sr) * sr) + sr // 50])  # keep 20 ms of tail
        gap = np.zeros(int(THINK_PAUSE_S * sr), dtype="<i2")
        speech = parts[0] if len(parts) == 1 else np.concatenate([parts[0], gap, parts[1]])
        audio = np.concatenate([np.zeros(int(LEAD_S * sr), "<i2"), speech, np.zeros(int(TAIL_S * sr), "<i2")])
        path = work / f"q{i:02d}.wav"
        with wave.open(str(path), "wb") as w:
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(sr)
            w.writeframes(audio.tobytes())
        text = f"{first} {second}" if second else first
        items.append({"wav": str(path), "text": text, "speech_end_s": LEAD_S + len(speech) / sr})
        # Same audio through the old batch path, for the accuracy comparison.
        stt = client.post(f"{base}/voice/stt", json={"audio_b64": base64.b64encode(path.read_bytes()).decode()}).json()
        batch_wer[i] = round(wer(text, stt.get("text", "")), 3)

    conf = {"url": base, "llm": cfg, "job_context": context, "skip_llm": args.skip_llm, "items": items}
    log_path = RESULTS / f"{stamp}-replay.log"
    tauri = BACKEND.parent / "src-tauri"
    exe = tauri / "target" / "debug" / "examples" / "copilot_replay.exe"
    if not args.no_build:
        # A running `npm run tauri:dev` holds cargo's build lock for as long as
        # the app is open — this then waits; use --no-build to run the last build.
        print("building the copilot_replay example (cargo)…")
        subprocess.run(["cargo", "build", "-q", "--example", "copilot_replay"], cwd=tauri, check=True)
    if not exe.exists():
        raise SystemExit(f"{exe} not found — run without --no-build once")
    print(f"replaying {len(items)} questions in real time through the Rust pipeline…")
    rows = []
    with open(log_path, "w", encoding="utf-8") as log:
        proc = subprocess.Popen(
            [str(exe)], cwd=tauri, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=log, text=True, encoding="utf-8",
        )
        proc.stdin.write(json.dumps(conf))
        proc.stdin.close()
        for line in proc.stdout:
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            row["wer"] = round(wer(row["text"], row.get("transcript") or ""), 3)
            row["batch_wer"] = batch_wer.get(row["i"])
            row["two_part"] = row["i"] >= len(plan) - len(TWO_PART)
            if not args.keep_answers:
                row.pop("answer", None)
            rows.append(row)
            print(f"[{row['i'] + 1:2d}/{len(items)}] stop→1st token={row.get('stop_to_first_token_ms')}ms "
                  f"endpoint={row.get('endpoint_ms')}ms asr_wait={row.get('asr_wait_ms')}ms "
                  f"ttft={row.get('ttft_ms')}ms restarts={row.get('restarts')} "
                  f"wer={row['wer']} (batch {row['batch_wer']}) {row.get('error') or ''}")
        proc.wait()
    if proc.returncode:
        print(f"copilot_replay exited {proc.returncode} — see {log_path}")
    if not args.keep_answers:
        for f in work.glob("*.wav"):
            f.unlink()
        work.rmdir()
    return rows


def pct(values: list[float], p: float):
    vals = sorted(v for v in values if v is not None)
    if not vals:
        return None
    if len(vals) == 1:
        return vals[0]
    return round(statistics.quantiles(vals, n=100, method="inclusive")[int(p) - 1], 3)


# ── main ─────────────────────────────────────────────────────────────────────
def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--questions", type=int, default=len(QUESTIONS), help="how many questions (max 20)")
    ap.add_argument("--provider", help="gemini | openai | anthropic (default: your Settings choice)")
    ap.add_argument("--model", help="pin one model, e.g. gemini-2.5-flash for the old baseline")
    ap.add_argument("--job", default="", help="job index or company/role substring (default: first with a JD)")
    ap.add_argument("--skip-stt", action="store_true", help="skip speech (TTS + STT)")
    ap.add_argument("--skip-llm", action="store_true", help="skip copilot answers (no API spend)")
    ap.add_argument("--no-warm", action="store_true", help="don't pre-warm the LLM channel like the app does")
    ap.add_argument("--keep-answers", action="store_true", help="save answer text in the results file")
    ap.add_argument("--pipeline", action="store_true",
                    help="Phase 2 gate: replay audio through the Rust live-listening pipeline")
    ap.add_argument("--no-build", action="store_true",
                    help="with --pipeline: run the last-built copilot_replay example instead of rebuilding")
    args = ap.parse_args()
    if args.pipeline and args.skip_stt:
        raise SystemExit("--pipeline needs speech (drop --skip-stt)")

    questions = QUESTIONS[: max(1, min(args.questions, len(QUESTIONS)))]
    cfg = llm_config(args)
    label, context = job_context(args.job)
    RESULTS.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    token = secrets.token_hex(16)

    print(f"provider={cfg['provider']} model={cfg['model'] or '(registry)'} "
          f"job={label or '(none)'} context_chars={len(context)} questions={len(questions)}")
    sidecar_log = RESULTS / f"{stamp}-sidecar.log"
    proc, base, boot_s = start_sidecar(token, sidecar_log)
    print(f"sidecar ready in {boot_s:.2f}s at {base}")
    report: dict = {"stamp": stamp, "provider": cfg["provider"], "model_pin": cfg["model"],
                    "job": label, "context_chars": len(context), "boot_s": round(boot_s, 2), "rows": []}
    rows = report["rows"]
    try:
        with httpx.Client(timeout=300) as client:
            if not args.skip_stt:
                t0 = time.perf_counter()
                prep = client.post(f"{base}/voice/prepare",
                                   json={"text": "", "engine": "piper", "stt_engine": "whisper"}).json()
                report["stt_warm_s"] = round(time.perf_counter() - t0, 2)
                print(f"speech warm in {report['stt_warm_s']}s (ready={prep.get('ready')})")

            if not args.skip_llm:
                key_field = f"{cfg['provider']}_api_key"
                if not cfg.get(key_field):
                    raise SystemExit(f"No {cfg['provider']} key found (Credential Manager entry "
                                     f"'{key_field}.InterPrep' or env var) — add it in Settings, "
                                     "or run with --skip-llm.")
                if not args.no_warm:
                    # Mirrors the app: Rust seeds config at startup, which warms
                    # the copilot's model client in the background. The sidecar
                    # logs the outcome either way; wait for that line.
                    client.post(f"{base}/config/seed", json={"llm": cfg},
                                headers={"X-InterPrep-Token": token})
                    t0, outcome = time.time(), "timed out"
                    while time.time() - t0 < 90:
                        log = sidecar_log.read_text(encoding="utf-8", errors="replace")
                        if "LLM channel warmed" in log:
                            outcome = "ok"
                            break
                        if "LLM warmup failed" in log:
                            outcome = log.split("LLM warmup failed", 1)[1].splitlines()[0]
                            outcome = outcome.split("):", 1)[-1].strip(" :")
                            break
                        time.sleep(0.25)
                    report["llm_warm_s"] = round(time.time() - t0, 2)
                    print(f"LLM channel warm in {report['llm_warm_s']}s ({outcome})")
                    if outcome != "ok" and "rejected" in outcome:
                        raise SystemExit(f"Stopping: {outcome}")

            if args.pipeline:
                rows.extend(run_pipeline(args, client, base, cfg, context, stamp))
            for i, q in enumerate([] if args.pipeline else questions, 1):
                row: dict = {"q": q}
                heard = q
                if not args.skip_stt:
                    tts = client.post(f"{base}/voice/tts", json={"text": q, "engine": "piper"}).json()
                    if "audio_b64" in tts:
                        t0 = time.perf_counter()
                        stt = client.post(f"{base}/voice/stt", json={"audio_b64": tts["audio_b64"]}).json()
                        row["stt_ms"] = round((time.perf_counter() - t0) * 1000)
                        heard = stt.get("text", "") or q
                        row["wer"] = round(wer(q, stt.get("text", "")), 3)
                    else:
                        row["stt_error"] = tts.get("error", "tts failed")
                if not args.skip_llm:
                    try:
                        before = usage(client, base, token)
                        ans = stream_answer(client, base, {
                            "message": heard, "job_context": context, "history": [],
                            "mode": "copilot", "llm": cfg, "documents": [],
                        })
                        row.update(usage_delta(before, usage(client, base, token), "chat:copilot"))
                        if not args.keep_answers:
                            ans.pop("answer")
                        row.update(ans)
                    except httpx.HTTPError as exc:
                        row["error"] = f"{type(exc).__name__}: {exc}"
                        if proc.poll() is not None:
                            rows.append(row)
                            print(f"sidecar died (exit {proc.returncode}) — see {sidecar_log}")
                            break
                rows.append(row)
                print(f"[{i:2d}/{len(questions)}] stt={row.get('stt_ms', '-')}ms wer={row.get('wer', '-')} "
                      f"ttft={row.get('ttft_ms', '-')}ms total={row.get('total_ms', '-')}ms "
                      f"in={row.get('input_tokens', '-')} out={row.get('output_tokens', '-')} "
                      f"{row.get('model', '')} {row.get('error', '')}")
    finally:
        stop_sidecar(proc)

    summary = {}
    for key in ("stop_to_first_token_ms", "endpoint_ms", "asr_wait_ms", "restarts", "batch_wer",
                "stt_ms", "wer", "ttft_ms", "total_ms", "input_tokens", "cached_input_tokens",
                "output_tokens", "answer_words"):
        vals = [r.get(key) for r in rows if r.get(key) is not None]
        if vals:
            summary[key] = {"p50": pct(vals, 50), "p90": pct(vals, 90), "mean": round(statistics.mean(vals), 3)}
    report["summary"] = summary
    out = RESULTS / f"{stamp}.json"
    out.write_text(json.dumps(report, indent=2), encoding="utf-8")

    print("\nmetric                 p50        p90       mean")
    for key, s in summary.items():
        print(f"{key:20s} {str(s['p50']):>8s} {str(s['p90']):>10s} {str(s['mean']):>10s}")
    if args.pipeline and "stop_to_first_token_ms" in summary:
        p50 = summary["stop_to_first_token_ms"]["p50"]
        w, bw = summary["wer"]["mean"], summary.get("batch_wer", {}).get("mean")
        two = [r["stop_to_first_token_ms"] for r in rows if r.get("two_part") and r.get("stop_to_first_token_ms")]
        print(f"\nPhase 2 gate: p50 stop→first token {p50} ms (target ≤ 1000) → {'PASS' if p50 <= 1000 else 'FAIL'}; "
              f"WER {w} vs batch {bw} → {'PASS' if bw is not None and w <= bw + 0.005 else 'CHECK'}; "
              f"two-part p50 {pct(two, 50) if two else '-'} ms")
    print(f"\nboot {report['boot_s']}s · saved {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
