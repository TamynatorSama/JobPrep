"""Condensed job brief — the copilot's and the mock interviewer's job context
(Phase 3 token diet).

The app mirrors the active job's full context into every copilot answer: the
JD (1.5k chars) + company-research dossier (6k) + resume (up to 8k), ~3,000-4,000 tokens
re-sent with every question. One fast-tier call condenses it to ~3k chars
(role, company, candidate, every name and number kept), cached by content hash
in memory and on disk, so each question costs ~1,600 input tokens instead of
~3,400. The mock interviewer gets the same brief instead of the raw JD + resume,
which also gives it the company research it used to lack.

Rust asks for the brief (POST /chat/brief) a moment after the selected job's
context changes, so it's ready before the first question. A request that
misses the cache uses the full context as before and builds the brief in the
background for the next one.

The interviewer's "INTERVIEW SETUP" block rides at the end of its job context;
it's split off before the lookup so every mode shares one brief per job.
"""
from __future__ import annotations

import asyncio
import hashlib
import json
import os
import re
import threading
import time
from collections import OrderedDict
from pathlib import Path

import llm_provider as llm_factory
from models import LLMConfig

SETUP_MARKER = "\nINTERVIEW SETUP (obey strictly):"
_DOSSIER = re.compile(r"\n\nCompany Research Dossier:\n.*?(?=\n\nCandidate Resume|\Z)", re.S)

BUDGET_CHARS = 2600
_MIN_CHARS, _MAX_CHARS = 600, 4500
_CACHE_MAX = 24

_PROMPT = """\
Condense a candidate's interview materials into a compact brief. An interview
assistant will use the brief as its ONLY knowledge of the job, the company and
the candidate, so keep every fact and drop the words around them.

Write plain text with exactly these three headings:

THE ROLE
3-6 short lines: what the job is, the main responsibilities, the must-have
skills and tools, and any pay, hours, dates or duration, in the job
description's own words.

THE COMPANY
4-8 short lines from the research: what the company does and for whom, its
products, business model, stage and size, mission and values, recent news, and
anything a candidate could cite to show why they want to work there.

THE CANDIDATE
Everything the candidate would draw on to talk about themselves:
- Education: school, degree, dates (and GPA or honors if given).
- Each job: title, employer, dates, then 1-3 lines of what they built or did.
- Each project: name, what it does, the stack, the outcome.
- Skills: one comma-separated line.
- Awards, publications or leadership: one line each.

Rules:
- Copy names, titles, numbers, dates and technologies exactly as written. Never
  add, infer, round or embellish anything; keep every metric.
- No contact details, URLs or filler. Short lines; no markdown except "- " at
  the start of a line.
- A section with no source material gets "(not provided)" under its heading.
- Stay under {budget} characters in total, and spend most of them on THE
  CANDIDATE.

=== MATERIALS ===
{materials}
"""

_lock = threading.Lock()
_cache: OrderedDict[str, str] | None = None
_inflight: dict[str, asyncio.Future] = {}
# Context key -> when its last build failed. A miss doesn't retry it for
# RETRY_AFTER_S, so a broken build can't add a call to every question.
_failed: dict[str, float] = {}
RETRY_AFTER_S = 300


def _path() -> Path:
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or "."
    d = Path(base) / "InterPrep"
    d.mkdir(parents=True, exist_ok=True)
    return d / "job_briefs.json"


def _entries() -> OrderedDict[str, str]:
    global _cache
    if _cache is None:
        try:
            _cache = OrderedDict(json.loads(_path().read_text(encoding="utf-8")))
        except (FileNotFoundError, ValueError, OSError):
            _cache = OrderedDict()
    return _cache


def _save() -> None:
    path = _path()
    tmp = path.with_suffix(".json.tmp")
    try:
        tmp.write_text(json.dumps(_entries()), encoding="utf-8")
        os.replace(tmp, path)
    except OSError:
        pass  # a lost write only costs one rebuild


def _key(base: str) -> str:
    """Cache key: the context plus the prompt that condenses it, so editing the
    prompt rebuilds every brief."""
    return hashlib.sha1(f"{_PROMPT}\x00{BUDGET_CHARS}\x00{base.strip()}".encode("utf-8")).hexdigest()


def split_setup(job_context: str) -> tuple[str, str]:
    """(job context, the interviewer's setup block or "")."""
    i = job_context.find(SETUP_MARKER)
    return (job_context, "") if i < 0 else (job_context[:i], job_context[i:])


def _header(base: str) -> str:
    """The "Company: / Role: / Location:" lines, kept verbatim above the brief."""
    return base.strip().split("\n\n", 1)[0]


def lookup(base: str) -> str | None:
    with _lock:
        entries = _entries()
        brief = entries.get(_key(base))
        if brief is not None:
            entries.move_to_end(_key(base))
        return brief


def _numbers(text: str) -> set[str]:
    """Multi-digit numbers, punctuation stripped ("200,000+" == "200000"), for
    the build log's rough check that metrics survived the condensing."""
    return {n for n in (re.sub(r"\D", "", m) for m in re.findall(r"\d[\d.,]*", text)) if len(n) >= 2}


async def _build(cfg: LLMConfig, base: str, key: str) -> str:
    llm_factory.set_feature("job_brief")
    head = _header(base)
    materials = base.strip()[len(head):].strip()
    if not materials:
        return ""
    t0 = time.monotonic()
    text, model = await llm_factory.generate_raw(
        cfg, _PROMPT.format(budget=BUDGET_CHARS, materials=materials),
        tier="fast", temperature=0.2,
    )
    body = (text or "").strip()
    if not (_MIN_CHARS <= len(body) <= _MAX_CHARS) or "THE CANDIDATE" not in body:
        print(f"[brief] rejected {len(body)}-char brief from {model}", flush=True)
        return ""
    brief = f"{head}\n\n{body}"
    src, kept = _numbers(materials), _numbers(body)
    print(f"[brief] {len(base)} -> {len(brief)} chars in {time.monotonic() - t0:.1f}s on {model} "
          f"(numbers kept {len(src & kept)}/{len(src)})", flush=True)
    with _lock:
        entries = _entries()
        entries[key] = brief
        while len(entries) > _CACHE_MAX:
            entries.popitem(last=False)
        _save()
    return brief


def _start(cfg: LLMConfig, base: str) -> asyncio.Future:
    """The in-flight build for this context, starting one if none is running.
    `_inflight` holds the only reference until it finishes."""
    key = _key(base)
    fut = _inflight.get(key)
    if fut is None:
        fut = asyncio.ensure_future(_build(cfg, base, key))
        _inflight[key] = fut

        def done(f: asyncio.Future) -> None:
            _inflight.pop(key, None)
            err = None if f.cancelled() else f.exception()
            if err:
                print(f"[brief] build failed: {err}", flush=True)
            if f.cancelled() or err or not f.result():
                _failed[key] = time.monotonic()
            else:
                _failed.pop(key, None)
        fut.add_done_callback(done)
    return fut


async def ensure(cfg: LLMConfig, base: str) -> str:
    """The brief for this job context, building it once if needed. "" when the
    build fails — callers then keep using the full context."""
    if not base.strip():
        return ""
    hit = lookup(base)
    if hit is not None:
        return hit
    try:
        return await asyncio.shield(_start(cfg, base))
    except Exception:  # noqa: BLE001 — logged by _start; the brief is an optimization
        return ""


def context_for(cfg: LLMConfig, job_context: str, mode: str) -> str:
    """The job context to put in the prompt for a copilot / interviewer turn:
    the brief when it's cached, else what was sent before Phase 3 (and the
    brief starts building for the next turn)."""
    base, setup = split_setup(job_context)
    brief = lookup(base)
    if brief is None:
        failed_at = _failed.get(_key(base))
        if base.strip() and (failed_at is None or time.monotonic() - failed_at > RETRY_AFTER_S):
            _start(cfg, base)
        if mode == "interviewer":
            # The interviewer never had the dossier; don't add it on a miss.
            base = _DOSSIER.sub("", base)
        return base + setup
    return brief + setup
