"""Token-diet session harness — the Phase 3 gate (see the InterPrep Copilot
Upgrade Blueprint): one scripted prep day for one job, run against a sidecar
it spawns, metering every LLM call per feature.

The script, in order, with payloads built exactly the way the app builds them:
  0. brief      — the condensed job brief Rust requests when a job is selected
  1. coach      — 6 coach-chat turns (job context + RAG over resume + chats)
  2. cheatsheet — a manual build (full rebuild)
  3. interview  — a mock interview: hidden "begin" turn, 8 scripted candidate
                  answers, then "end interview" for the feedback turn
  4. cheatsheet — the automatic rebuild that follows a mock interview
  5. copilot    — the 20 latency-harness questions in copilot mode
  6. extract    — /inbox/extract on a synthetic job page (the JD wrapped in
                  ~12k chars of site chrome), the extension's fallback path

Reports input / cached / output tokens per feature and a "billed" figure that
counts cached input at CACHED_RATE of the normal input price. Answers, the
interviewer's questions and both cheatsheets are saved so a before/after pair of
runs can be compared side by side (bench/judge_session.py).

Payloads mirror App.tsx as of Phase 3 (the interviewer sends the copilot's full
context, the cheatsheet carries `previous` + `incremental`). The baseline run
(results/20261001-184340-baseline-session.json) used the pre-Phase 3 payloads.

  cd backend
  .\\.venv\\Scripts\\python.exe bench\\token_session.py --provider gemini --label baseline
  .\\.venv\\Scripts\\python.exe bench\\token_session.py --only copilot,coach

LLM runs spend real tokens (roughly 150-200k input tokens on the fast tiers).
"""
from __future__ import annotations

import argparse
import json
import os
import secrets
import sys
import time
from datetime import datetime
from pathlib import Path

import httpx

sys.path.insert(0, str(Path(__file__).resolve().parent))
import latency_harness as lh  # noqa: E402

# Cached input is billed at a fraction of the normal input price: 10% on
# OpenAI GPT-5.x and Anthropic cache reads, and on Gemini's implicit cache for
# 2.5+ models. One rate keeps the report simple; Gemini Flash rarely caches
# these prompts at all (4,096-token floor), so it barely moves the total.
CACHED_RATE = 0.10

SECTIONS = ("brief", "coach", "cheatsheet", "interview", "copilot", "extract")

COACH_TURNS = [
    "What should I focus on to prepare for this interview?",
    "Help me shape my answer to 'tell me about yourself' for this role.",
    "Which technical topics are they most likely to ask about, given the job description?",
    "Outline a STAR story from my resume for a time I shipped something under pressure.",
    "What should I ask them at the end of the interview?",
    "Give me my top three talking points, one line each.",
]

BEGIN_INTERVIEW = "Begin the interview now. Ask your first question."
END_INTERVIEW = "Let's stop here. End the interview and give me feedback."

# Scripted candidate answers: plausible, deliberately generic (no invented
# employers), so the interviewer reacts the same way run to run.
INTERVIEW_ANSWERS = [
    "Sure. I'm a computer science student who's spent the last two years building "
    "LLM-backed tools, mostly in Python and TypeScript. Most recently I built a desktop "
    "interview-prep app with a FastAPI backend and a Rust shell, and I'm looking for a "
    "team where I can ship AI features to real users.",
    "I'd start by pinning down what 'good' looks like: a small eval set of real inputs "
    "with expected outputs. Then I'd try the simplest prompt, measure it, and only add "
    "retrieval or tools if the evals show the model is missing context.",
    "The hardest bug I hit was a streaming answer that stalled for minutes. The first "
    "token never arrived when one provider was slow, and our client waited 300 seconds. "
    "I added a time-to-first-token deadline and failover to the next model, which fixed it.",
    "I'd cache by content hash so unchanged text is never re-embedded, keep the corpus "
    "per job since it's small, and use plain cosine similarity instead of a vector DB.",
    "We disagreed about whether to rewrite a module or patch it. I wrote down both "
    "options with the time cost and risk, we agreed on the patch plus a test, and the "
    "rewrite went on the backlog with a clear trigger.",
    "I'd log tokens per feature first, so every cut is measured. Then I'd shrink what we "
    "send on every request, since that's where most of the spend is.",
    "I'd add a canary: route a small share of traffic to the new version, compare error "
    "rates and latency, and roll back automatically if they drift.",
    "Mostly the chance to ship AI features end to end with a small team, and the fact "
    "that your product is used by people who need it to work every time.",
]

# Site chrome wrapped around the JD for the extract test: navigation, cookie
# banner, related jobs, footer — what a real job page's innerText carries.
_NAV = ("Home\nJobs\nCompanies\nSalaries\nSign in\nJoin now\nPost a job\n"
        "Search jobs\nLocation\nRemote\nDate posted\nExperience level\n") * 6
_COOKIE = ("We use cookies to improve your experience, analyze site traffic and "
           "personalize content. By clicking Accept all you agree to our use of cookies. "
           "Manage preferences\nAccept all\nReject all\n")
_RELATED = "".join(
    f"Related job {i}\nSoftware Engineer {i}\nSome Company {i}\nRemote · Full-time\n"
    f"Posted {i} days ago\nEasy apply\nSave\n" for i in range(1, 60))
_FOOTER = ("About\nCareers\nPress\nHelp center\nPrivacy\nTerms\nAccessibility\n"
           "Cookie policy\nCopyright 2026\nLanguage: English\n") * 8


def billed(row: dict) -> float:
    cached = row.get("cached_input_tokens", 0)
    return row.get("input_tokens", 0) - cached + cached * CACHED_RATE


def pick_job(selector: str) -> tuple[dict | None, list]:
    base = Path(os.environ.get("LOCALAPPDATA", ".")) / "InterPrep"
    jobs = json.loads((base / "jobs.json").read_text(encoding="utf-8"))
    try:
        resumes = json.loads((base / "resumes.json").read_text(encoding="utf-8"))
    except Exception:
        resumes = []
    live = [j for j in jobs if not j.get("archived")]
    if selector.isdigit():
        return (live[int(selector)] if int(selector) < len(live) else None), resumes
    if selector:
        return next((j for j in live if selector.lower() in
                     f"{j.get('company', '')} {j.get('role', '')}".lower()), None), resumes
    return next((j for j in live if (j.get("jobDescription") or "").strip()), live[0] if live else None), resumes


def done_msgs(chat: dict) -> list[dict]:
    return [m for m in chat.get("messages", [])
            if not m.get("streaming") and (m.get("content") or "").strip()]


def research_chat(job: dict) -> dict | None:
    return next((c for c in job.get("chats", [])
                 if c.get("id") == f"c-research-{job.get('id')}" or c.get("title") == "Company Research"), None)


def resume_text(job: dict, resumes: list) -> tuple[str, str]:
    tailored = (job.get("tailoredResume") or "").strip()
    if tailored:
        return tailored, "Tailored Resume"
    last = resumes[-1] if resumes else {}
    return (last.get("text") or "").strip(), last.get("name") or "Resume"


# App.tsx formatInterviewSetup(DEFAULT_INTERVIEW_CONFIG), appended to the
# interviewer's job context.
INTERVIEW_SETUP = ("\nINTERVIEW SETUP (obey strictly):\n- Focus: Full Loop\n"
                   "- Candidate level / difficulty: Senior\n- Interviewer tone: Neutral\n"
                   "- Length: Standard (8–12 questions)")


def coach_job_context(job: dict, resumes: list) -> str:
    """App.tsx onSendMessage's jobContext for the coach (the interviewer sends
    the copilot's full context + INTERVIEW_SETUP)."""
    text, name = resume_text(job, resumes)
    parts = [
        f"Company: {job.get('company', '')}",
        f"Role: {job.get('role', '')}",
        f"Location: {job['location']}" if job.get("location") else "",
        f"\nJob Description:\n{job['jobDescription'][:1500]}" if job.get("jobDescription") else "",
        f"\nCandidate Resume ({name}):\n{text[:4000]}" if text else "",
    ]
    return "\n".join(p for p in parts if p)


def rag_docs(job: dict, resumes: list, skip_chat_id: str = "") -> list[dict]:
    """App.tsx onSendMessage's RAG corpus (coach only)."""
    docs = []
    text, _ = resume_text(job, resumes)
    if text:
        docs.append({"source": "resume", "text": text})
    for c in job.get("chats", []):
        if c.get("id") == skip_chat_id:
            continue
        is_research = c.get("title") == "Company Research"
        body = "\n\n".join(f"{'Candidate' if m['role'] == 'user' else 'Assistant'}: {m['content']}"
                           for m in done_msgs(c))
        if body:
            docs.append({"source": "company_research" if is_research else f"chat: {c.get('title')}", "text": body})
    return docs


def cheatsheet_payload(job: dict, resumes: list, extra_chats: list[dict], previous: dict | None,
                       incremental: bool) -> dict:
    """App.tsx generateCheatsheet's payload (+ Phase 3's previous/incremental)."""
    research = research_chat(job)
    company_research = "\n\n".join(m["content"] for m in done_msgs(research or {}) if m.get("role") == "ai")
    documents = []
    for c in job.get("chats", []) + extra_chats:
        if research and c.get("id") == research.get("id"):
            continue
        body = "\n\n".join(f"{'Candidate' if m['role'] == 'user' else 'Assistant'}: {m['content']}"
                           for m in done_msgs(c) if "--- INTERVIEW COMPLETE ---" not in m["content"])
        if body:
            documents.append({"id": c.get("id", ""), "source": f"chat: {c.get('title')}", "text": body})
    text, _ = resume_text(job, resumes)
    return {
        "company": job.get("company", ""), "role": job.get("role", ""), "location": job.get("location", ""),
        "job_description": job.get("jobDescription") or "", "resume": text,
        "company_research": company_research, "documents": documents,
        "previous_markdown": (previous or {}).get("markdown", ""),
        "previous": previous, "incremental": incremental,
    }


class Meter:
    def __init__(self, client: httpx.Client, base: str, token: str):
        self.client, self.base, self.token = client, base, token
        self.sections: dict[str, dict] = {}

    def snap(self) -> dict:
        try:
            return lh.usage(self.client, self.base, self.token)
        except httpx.TransportError:  # a stale keep-alive connection; retry once
            return lh.usage(self.client, self.base, self.token)

    def add(self, section: str, before: dict, after: dict) -> None:
        sec = self.sections.setdefault(section, {"calls": 0, "input_tokens": 0,
                                                 "cached_input_tokens": 0, "output_tokens": 0, "models": {}})
        for key, row in after.items():
            prev = before.get(key, {})
            calls = row["calls"] - prev.get("calls", 0)
            if calls <= 0:
                continue
            sec["calls"] += calls
            sec["models"][key[1]] = sec["models"].get(key[1], 0) + calls
            for k in ("input_tokens", "cached_input_tokens", "output_tokens"):
                sec[k] += row[k] - prev.get(k, 0)


def stream(client: httpx.Client, base: str, body: dict) -> dict:
    ans = lh.stream_answer(client, base, body)
    if ans["error"]:
        print(f"    error: {ans['error']}")
    return ans


def run_coach(client, base, cfg, job, resumes, meter, out) -> None:
    ctx = coach_job_context(job, resumes)
    docs = rag_docs(job, resumes, skip_chat_id="bench-coach")
    history: list[dict] = []
    turns = []
    for i, msg in enumerate(COACH_TURNS, 1):
        before = meter.snap()
        ans = stream(client, base, {"message": msg, "job_context": ctx, "history": history,
                                    "mode": "coach", "llm": cfg, "documents": docs})
        meter.add("coach", before, meter.snap())
        history += [{"role": "user", "content": msg}, {"role": "assistant", "content": ans["answer"]}]
        turns.append({"q": msg, "a": ans["answer"], "ttft_ms": ans["ttft_ms"]})
        print(f"  coach {i}/{len(COACH_TURNS)} ttft={ans['ttft_ms']}ms words={ans['answer_words']}")
    out["coach"] = turns


def run_interview(client, base, cfg, full_ctx, meter, out) -> dict:
    ctx = full_ctx + INTERVIEW_SETUP
    history: list[dict] = []
    transcript = []
    for i, msg in enumerate([BEGIN_INTERVIEW] + INTERVIEW_ANSWERS + [END_INTERVIEW]):
        before = meter.snap()
        ans = stream(client, base, {"message": msg, "job_context": ctx, "history": history,
                                    "mode": "interviewer", "llm": cfg, "documents": []})
        meter.add("interview", before, meter.snap())
        history += [{"role": "user", "content": msg}, {"role": "assistant", "content": ans["answer"]}]
        transcript.append({"candidate": msg, "interviewer": ans["answer"]})
        print(f"  interview {i + 1}/{len(INTERVIEW_ANSWERS) + 2} ttft={ans['ttft_ms']}ms words={ans['answer_words']}")
    out["interview"] = transcript
    # The finished thread, as the app stores it (the hidden begin turn included).
    messages = [{"role": "user" if h["role"] == "user" else "ai", "content": h["content"]} for h in history]
    return {"id": "bench-interview", "title": "Mock Interview", "mode": "interviewer", "messages": messages}


def build_cheatsheet(client, base, cfg, payload, meter, label) -> dict | None:
    before = meter.snap()
    t0 = time.perf_counter()
    resp = client.post(f"{base}/cheatsheet/build", json={**payload, "llm": cfg}, timeout=300).json()
    meter.add("cheatsheet", before, meter.snap())
    print(f"  cheatsheet ({label}) {round((time.perf_counter() - t0) * 1000)}ms "
          f"{resp.get('error') or str(len(resp.get('stories') or [])) + ' stories'}")
    return None if resp.get("error") else resp


def prefetch_brief(client, base, cfg, context, meter, out) -> None:
    """Rust asks for the job brief when the selected job's context changes, long
    before any copilot question or mock interview. An older sidecar 404s."""
    before = meter.snap()
    t0 = time.perf_counter()
    try:
        r = client.post(f"{base}/chat/brief", json={"job_context": context, "llm": cfg}, timeout=180)
        brief = r.json() if r.status_code == 200 else {}
    except httpx.HTTPError:
        brief = {}
    meter.add("brief", before, meter.snap())
    if brief:
        print(f"  job brief {round((time.perf_counter() - t0) * 1000)}ms "
              f"{brief.get('chars', '?')} chars (from {len(context)})")
        out["brief"] = brief


def run_copilot(client, base, cfg, context, meter, out) -> None:
    rows = []
    for i, q in enumerate(lh.QUESTIONS, 1):
        before = meter.snap()
        ans = stream(client, base, {"message": q, "job_context": context, "history": [],
                                    "mode": "copilot", "llm": cfg, "documents": []})
        after = meter.snap()
        meter.add("copilot", before, after)
        rows.append({"q": q, "a": ans["answer"], "ttft_ms": ans["ttft_ms"],
                     **lh.usage_delta(before, after, "chat:copilot")})
        print(f"  copilot {i:2d}/{len(lh.QUESTIONS)} ttft={ans['ttft_ms']}ms in={rows[-1]['input_tokens']} "
              f"words={ans['answer_words']}")
    out["copilot"] = rows


def run_extract(client, base, token, job, meter, out) -> None:
    page = _NAV + _COOKIE + f"{job.get('role', '')}\n{job.get('company', '')}\n{job.get('location', '')}\n" \
        + (job.get("jobDescription") or "") + "\n" + _RELATED + _FOOTER
    before = meter.snap()
    r = client.post(f"{base}/inbox/extract", headers={"X-InterPrep-Token": token}, timeout=300,
                    json={"url": "https://jobs.example.com/view/123", "title": job.get("role", ""), "page_text": page})
    meter.add("extract", before, meter.snap())
    data = r.json() if r.status_code == 200 else {"error": r.text[:200]}
    jd = job.get("jobDescription") or ""
    got = data.get("job_description", "")
    print(f"  extract page={len(page)} chars → jd={len(got)} chars (source {len(jd)}) "
          f"role={data.get('role', '')!r} {data.get('error', '')}")
    out["extract"] = {"page_chars": len(page), **data}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--provider", help="gemini | openai | anthropic (default: your Settings choice)")
    ap.add_argument("--model", help="pin one model for every call")
    ap.add_argument("--job", default="", help="job index or company/role substring (default: first with a JD)")
    ap.add_argument("--only", default="", help=f"comma list of sections to run ({','.join(SECTIONS)})")
    ap.add_argument("--label", default="", help="tag for the results file, e.g. baseline / after")
    args = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    only = {s.strip() for s in args.only.split(",") if s.strip()} or set(SECTIONS)

    cfg = lh.llm_config(args)
    if not cfg.get(f"{cfg['provider']}_api_key"):
        raise SystemExit(f"No {cfg['provider']} key (Credential Manager '{cfg['provider']}_api_key.InterPrep').")
    job, resumes = pick_job(args.job)
    if not job:
        raise SystemExit("No job found in jobs.json")
    _, copilot_ctx = lh.job_context(args.job)
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S") + (f"-{args.label}" if args.label else "")
    lh.RESULTS.mkdir(parents=True, exist_ok=True)
    token = secrets.token_hex(16)
    sidecar_log = lh.RESULTS / f"{stamp}-session-sidecar.log"
    print(f"provider={cfg['provider']} job={job.get('role')} · {job.get('company')} sections={sorted(only)}")
    proc, base, boot_s = lh.start_sidecar(token, sidecar_log)
    print(f"sidecar ready in {boot_s:.2f}s")
    out: dict = {"stamp": stamp, "provider": cfg["provider"], "model_pin": cfg["model"],
                 "job": f"{job.get('role')} · {job.get('company')}", "cached_rate": CACHED_RATE}
    try:
        with httpx.Client(timeout=300) as client:
            client.post(f"{base}/config/seed", json={"llm": cfg}, headers={"X-InterPrep-Token": token})
            # Like the app: wait for the seeded warm-up so the first call isn't cold.
            t0 = time.time()
            while time.time() - t0 < 120:
                log = sidecar_log.read_text(encoding="utf-8", errors="replace")
                if "LLM channel warmed" in log or "LLM warmup failed" in log:
                    break
                time.sleep(0.25)
            print(f"LLM channel warm in {time.time() - t0:.1f}s")
            meter = Meter(client, base, token)
            previous = job.get("cheatsheet")
            if only & {"interview", "copilot"}:
                prefetch_brief(client, base, cfg, copilot_ctx, meter, out)
            if "coach" in only:
                run_coach(client, base, cfg, job, resumes, meter, out)
            if "cheatsheet" in only:
                previous = build_cheatsheet(client, base, cfg,
                                            cheatsheet_payload(job, resumes, [], previous, incremental=False),
                                            meter, "manual") or previous
                out["cheatsheet_manual"] = previous
            interview_chat = None
            if "interview" in only:
                interview_chat = run_interview(client, base, cfg, copilot_ctx, meter, out)
            if "cheatsheet" in only and interview_chat:
                out["cheatsheet_auto"] = build_cheatsheet(
                    client, base, cfg,
                    cheatsheet_payload(job, resumes, [interview_chat], previous, incremental=True),
                    meter, "auto")
            if "copilot" in only:
                run_copilot(client, base, cfg, copilot_ctx, meter, out)
            if "extract" in only:
                run_extract(client, base, token, job, meter, out)
    except Exception as exc:  # keep what ran; the report below still prints
        print(f"session stopped: {type(exc).__name__}: {exc}")
        out["stopped"] = repr(exc)
    finally:
        lh.stop_sidecar(proc)

    out["sections"] = meter.sections
    total = {"input_tokens": 0, "cached_input_tokens": 0, "output_tokens": 0, "billed": 0.0}
    print(f"\n{'section':12s} {'calls':>5s} {'input':>8s} {'cached':>8s} {'output':>7s} {'billed':>9s}  models")
    for name in SECTIONS:
        sec = meter.sections.get(name)
        if not sec:
            continue
        b = billed(sec)
        sec["billed"] = round(b)
        for k in ("input_tokens", "cached_input_tokens", "output_tokens"):
            total[k] += sec[k]
        total["billed"] += b
        print(f"{name:12s} {sec['calls']:5d} {sec['input_tokens']:8d} {sec['cached_input_tokens']:8d} "
              f"{sec['output_tokens']:7d} {round(b):9d}  {sec['models']}")
    total["billed"] = round(total["billed"])
    out["total"] = total
    print(f"{'total':12s} {'':5s} {total['input_tokens']:8d} {total['cached_input_tokens']:8d} "
          f"{total['output_tokens']:7d} {total['billed']:9d}")
    path = lh.RESULTS / f"{stamp}-session.json"
    path.write_text(json.dumps(out, indent=2), encoding="utf-8")
    print(f"\nsaved {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
