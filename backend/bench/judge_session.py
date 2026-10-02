"""Side-by-side quality check for two token_session.py runs — the "eval no
worse" half of the Phase 3 gate.

A judge model (smart tier, your Gemini key) sees the ground truth each output
could draw on — the uncondensed JD + research + resume, plus the coach's whole
retrieval corpus and the mock interview's candidate answers where those apply —
and, blind and in random order, the two runs' outputs for the same input:
  * each of the 20 copilot answers
  * each of the 6 coach replies
  * the whole mock-interview transcript (one judgment)
  * the automatic post-interview cheatsheet (one judgment)
It picks the better one (or a tie) and flags any fact that isn't in the
materials. "No worse" = the new run loses no more comparisons than it wins, and
invents no more facts than the old one.

  cd backend
  .\\.venv\\Scripts\\python.exe bench\\judge_session.py results\\<old>-session.json results\\<new>-session.json
"""
from __future__ import annotations

import argparse
import asyncio
import json
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import latency_harness as lh  # noqa: E402

_PROMPT = """You are judging two AI outputs produced for the same input in an interview-prep app.
The candidate's real materials are below; they are the ONLY source of truth about the
candidate, the job and the company.

=== MATERIALS ===
{materials}

=== TASK THE OUTPUTS WERE FOR ===
{task}

=== INPUT ===
{input}

=== OUTPUT A ===
{a}

=== OUTPUT B ===
{b}

Judge which output is better at the task: {criteria}
Also list facts each output states about the candidate, the job or the company that are
NOT supported by the materials (invented employers, numbers, projects, products, claims).
General knowledge and reasoning are fine; only unsupported specifics count.

Return ONLY JSON: {{"winner": "A" | "B" | "tie", "reason": "one sentence",
"invented_a": ["..."], "invented_b": ["..."]}}"""

TASKS = {
    "copilot": ("a live interview copilot: the candidate reads the answer aloud in first person",
                "answers the question asked, sounds like a real person talking, uses the "
                "candidate's real background where it helps, and is accurate."),
    "coach": ("an interview coach chatting with the candidate",
              "helpful, specific to this job and candidate, accurate, and clear."),
    "interview": ("a mock interviewer running a realistic interview for this role and company",
                  "realistic, specific to the role and company, probing, and fair final feedback."),
    "cheatsheet": ("an interview cheatsheet (STAR stories, facts, questions to ask) refreshed after a "
                   "mock interview",
                   "strong, specific, accurate stories and facts; sharp questions; nothing invented."),
}


def pairs(old: dict, new: dict) -> list[tuple[str, str, str, str]]:
    """(kind, input, old output, new output)."""
    out = []
    for o, n in zip(old.get("copilot", []), new.get("copilot", [])):
        if o.get("a") and n.get("a"):
            out.append(("copilot", o["q"], o["a"], n["a"]))
    for o, n in zip(old.get("coach", []), new.get("coach", [])):
        if o.get("a") and n.get("a"):
            out.append(("coach", o["q"], o["a"], n["a"]))
    if old.get("interview") and new.get("interview"):
        fmt = lambda t: "\n\n".join(f"Candidate: {x['candidate']}\nInterviewer: {x['interviewer']}" for x in t)
        out.append(("interview", "(scripted candidate answers; same in both)",
                    fmt(old["interview"]), fmt(new["interview"])))
    if old.get("cheatsheet_auto") and new.get("cheatsheet_auto"):
        out.append(("cheatsheet", "(the job's conversations, including the mock interview)",
                    old["cheatsheet_auto"].get("markdown", ""), new["cheatsheet_auto"].get("markdown", "")))
    return out


async def judge(cfg, materials: str, kind: str, inp: str, old_out: str, new_out: str, rng) -> dict:
    import llm_provider as llm_factory
    from models import LLMConfig

    new_is_a = rng.random() < 0.5
    a, b = (new_out, old_out) if new_is_a else (old_out, new_out)
    task, criteria = TASKS[kind]
    prompt = _PROMPT.format(materials=materials, task=task, input=inp, a=a, b=b, criteria=criteria)
    data, model = await llm_factory.generate_json(LLMConfig(**cfg), prompt, tier="smart", temperature=0.0)
    w = data.get("winner", "tie")
    verdict = "tie" if w not in ("A", "B") else ("new" if (w == "A") == new_is_a else "old")
    inv_new = data.get("invented_a" if new_is_a else "invented_b") or []
    inv_old = data.get("invented_b" if new_is_a else "invented_a") or []
    return {"kind": kind, "input": inp[:80], "verdict": verdict, "reason": data.get("reason", ""),
            "invented_new": inv_new, "invented_old": inv_old, "judge": model}


def materials_by_kind(selector: str) -> dict[str, str]:
    """What each kind of output could legitimately draw on — the judge's ground
    truth. Coach retrieval reaches the whole research corpus and other chats,
    and the cheatsheet reads the mock interview's (scripted) candidate answers."""
    import token_session as ts
    _, full = lh.job_context(selector)
    job, resumes = ts.pick_job(selector)
    corpus = "\n\n".join(f"[{d['source']}]\n{d['text']}" for d in ts.rag_docs(job, resumes))
    answers = "\n\n".join(f"Candidate: {a}" for a in ts.INTERVIEW_ANSWERS)
    return {
        "copilot": full,
        "interview": full,
        "coach": f"{ts.coach_job_context(job, resumes)}\n\n=== RETRIEVABLE NOTES ===\n{corpus}",
        "cheatsheet": f"{full}\n\n=== JOB'S CONVERSATIONS ===\n{corpus}\n\n"
                      f"=== MOCK INTERVIEW: CANDIDATE'S ANSWERS ===\n{answers}",
    }


async def main_async(args) -> int:
    old = json.loads(Path(args.old).read_text(encoding="utf-8"))
    new = json.loads(Path(args.new).read_text(encoding="utf-8"))
    cfg = lh.llm_config(argparse.Namespace(provider=args.provider, model=None))
    materials = materials_by_kind(args.job)
    rng = random.Random(7)
    results = []
    for kind, inp, o, n in pairs(old, new):
        try:
            r = await judge(cfg, materials[kind], kind, inp, o, n, rng)
        except Exception as exc:  # noqa: BLE001 — record and keep going
            r = {"kind": kind, "input": inp[:80], "verdict": "error", "reason": str(exc)[:200],
                 "invented_new": [], "invented_old": []}
        results.append(r)
        print(f"{kind:10s} {r['verdict']:5s} new_inv={len(r['invented_new'])} old_inv={len(r['invented_old'])} "
              f"| {inp[:50]} | {r['reason'][:110]}")
    print("\nkind        new  old  tie  invented(new/old)")
    for kind in TASKS:
        rs = [r for r in results if r["kind"] == kind]
        if rs:
            c = lambda v: sum(r["verdict"] == v for r in rs)
            print(f"{kind:10s} {c('new'):4d} {c('old'):4d} {c('tie'):4d}  "
                  f"{sum(len(r['invented_new']) for r in rs)}/{sum(len(r['invented_old']) for r in rs)}")
    wins = sum(r["verdict"] == "new" for r in results)
    losses = sum(r["verdict"] == "old" for r in results)
    inv_new = sum(len(r["invented_new"]) for r in results)
    inv_old = sum(len(r["invented_old"]) for r in results)
    ok = losses <= wins and inv_new <= inv_old
    print(f"\nnew won {wins}, lost {losses}, tied {sum(r['verdict'] == 'tie' for r in results)}; "
          f"invented facts new {inv_new} vs old {inv_old} -> {'NO WORSE' if ok else 'CHECK'}")
    out = Path(args.new).with_name(Path(args.new).stem + "-judged.json")
    out.write_text(json.dumps({"old": args.old, "new": args.new, "results": results}, indent=2), encoding="utf-8")
    print(f"saved {out}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("old")
    ap.add_argument("new")
    ap.add_argument("--provider", default="gemini")
    ap.add_argument("--job", default="")
    args = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    return asyncio.run(main_async(args))


if __name__ == "__main__":
    sys.exit(main())
