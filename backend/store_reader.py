"""Read-only (plus the answer-bank) access to the InterPrep data files.

These are the SAME JSON files the Rust shell writes under
``%LOCALAPPDATA%\\InterPrep\\`` (falling back to ``%APPDATA%`` exactly like the
Rust ``data_dir()`` does). The browser extension can't reach the Rust IPC layer,
so the Python sidecar reads the files directly and serves them over HTTP.

  * ``jobs.json``     — list of Job records (camelCase, written by jobs_store.rs)
  * ``resumes.json``  — list of master resumes (id/name/text/docx_b64)
  * ``qa_memory.json``— the answer bank this module owns (see remember()).

Only the answer bank is written here, with the same temp-file + rename atomic
strategy the Rust side uses, so a crash mid-write never corrupts it.
"""
from __future__ import annotations

import json
import os
import re
import tempfile
from pathlib import Path
from typing import Any


def data_dir() -> Path:
    base = os.environ.get("LOCALAPPDATA") or os.environ.get("APPDATA") or "."
    return Path(base) / "InterPrep"


def _read_json(name: str, default: Any) -> Any:
    path = data_dir() / name
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except FileNotFoundError:
        return default
    except (json.JSONDecodeError, OSError) as exc:
        print(f"[store_reader] read {path} failed: {exc!r}", flush=True)
        return default


# ── Jobs ─────────────────────────────────────────────────────────────────────

def _raw_jobs() -> list[dict]:
    raw = _read_json("jobs.json", [])
    return [j for j in raw if isinstance(j, dict)] if isinstance(raw, list) else []


def get_raw_job(job_id: str) -> dict | None:
    """Full job record (incl. chats) straight from jobs.json."""
    for j in _raw_jobs():
        if str(j.get("id")) == str(job_id):
            return j
    return None


def research_text(job: dict) -> str:
    """Pull the Company-Research write-up out of a job's chat threads.

    Company research is streamed into a ChatThread titled "Company Research"
    (see App.tsx); the analysis lives in that thread's assistant ("ai") messages.
    Any other thread whose title contains "research" is included too."""
    parts: list[str] = []
    for c in job.get("chats") or []:
        if not isinstance(c, dict):
            continue
        title = (c.get("title") or "")
        if "research" not in title.lower():
            continue
        body = "\n\n".join(
            m.get("content", "")
            for m in (c.get("messages") or [])
            if isinstance(m, dict) and m.get("role") == "ai" and m.get("content")
        ).strip()
        if body:
            parts.append(body)
    return "\n\n".join(parts).strip()


def _first_line(text: str) -> str:
    """First non-empty line — the candidate's name in our resume layout
    (mirrors resume_pdf.candidate_name without importing reportlab here)."""
    for line in (text or "").split("\n"):
        if line.strip():
            return line.strip()
    return ""


def load_jobs() -> list[dict]:
    """All jobs, trimmed to the fields the extension needs (no full chats).

    `hasResearch` / `hasTailoredResume` let the popup show what context is
    available for each job without shipping the whole corpus."""
    out = []
    for j in _raw_jobs():
        tailored = j.get("tailoredResume") or ""
        out.append({
            "id": j.get("id", ""),
            "company": j.get("company", ""),
            "role": j.get("role", ""),
            "location": j.get("location", ""),
            "url": j.get("url", ""),
            "jobDescription": j.get("jobDescription") or "",
            "tailoredResume": tailored,
            "hasTailoredResume": bool(tailored),
            "hasResearch": bool(research_text(j)),
            "archived": bool(j.get("archived", False)),
            # Pipeline status + avatar so the extension's job picker can show the
            # same status badge / colored monogram the desktop app does.
            "status": j.get("status", "Applied"),
            "avatar": j.get("avatar", ""),
            "avatarColor": j.get("avatarColor", ""),
            # Candidate name (resume header) for the autofill preview's profile card.
            "candidateName": _first_line(tailored),
        })
    return out


def get_job(job_id: str) -> dict | None:
    for j in load_jobs():
        if str(j.get("id")) == str(job_id):
            return j
    return None


# ── Resumes ──────────────────────────────────────────────────────────────────

def load_resumes() -> list[dict]:
    """Master resumes, without the bulky base64 docx blob."""
    raw = _read_json("resumes.json", [])
    if not isinstance(raw, list):
        return []
    out = []
    for r in raw:
        if not isinstance(r, dict):
            continue
        out.append({
            "id": r.get("id"),
            "name": r.get("name", ""),
            "text": r.get("text", ""),
        })
    return out


def read_resume_docx(job_id: str) -> bytes | None:
    """Bytes of the per-job tailored resume.docx, if it was generated.

    The desktop app writes it to %APPDATA%\\InterPrep\\applications\\<job_id>\\
    resume.docx (Roaming AppData — see save_resume_docx in lib.rs, which uses
    dirs::data_dir()). Returns None when there's no file yet."""
    base = os.environ.get("APPDATA")
    if not base:
        return None
    path = Path(base) / "InterPrep" / "applications" / str(job_id) / "resume.docx"
    try:
        return path.read_bytes()
    except OSError:
        return None


def resumes_text(resume_ids: list[int] | None) -> list[dict]:
    """Return [{name,text}] for the requested ids, or all when ids is empty."""
    all_resumes = load_resumes()
    if not resume_ids:
        return [{"name": r["name"], "text": r["text"]} for r in all_resumes]
    wanted = {str(i) for i in resume_ids}
    return [
        {"name": r["name"], "text": r["text"]}
        for r in all_resumes
        if str(r.get("id")) in wanted
    ]


# ── Answer bank (qa_memory.json) ─────────────────────────────────────────────

_MEMORY_FILE = "qa_memory.json"


def normalize_label(label: str) -> str:
    """Canonical key for a question so 'Years of experience*' and
    'years of experience' hit the same bank entry."""
    t = (label or "").strip().lower()
    t = t.rstrip("*:?").strip()        # drop required-markers / trailing punct
    t = re.sub(r"\s+", " ", t)
    return t


def load_memory() -> dict[str, dict]:
    raw = _read_json(_MEMORY_FILE, {})
    return raw if isinstance(raw, dict) else {}


def remember(label: str, value: str) -> None:
    """Upsert one answer into the bank, keyed by the normalized label."""
    key = normalize_label(label)
    if not key:
        return
    mem = load_memory()
    mem[key] = {"label": label, "value": value}
    _atomic_write_json(_MEMORY_FILE, mem)


def _atomic_write_json(name: str, data) -> None:
    d = data_dir()
    d.mkdir(parents=True, exist_ok=True)
    path = d / name
    fd, tmp = tempfile.mkstemp(dir=str(d), suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
        os.replace(tmp, path)
    except OSError as exc:
        print(f"[store_reader] write {path} failed: {exc!r}", flush=True)
        try:
            os.unlink(tmp)
        except OSError:
            pass


# ── Job-capture inbox (extension -> app handoff) ─────────────────────────────
# The extension can't reach the app's IPC, so a JD it captures is queued here.
# The app polls, creates the job + starts research, then acks it off the queue.

_INBOX_FILE = "inbox.json"


def load_inbox() -> list[dict]:
    raw = _read_json(_INBOX_FILE, [])
    return raw if isinstance(raw, list) else []


def add_inbox_item(item: dict) -> None:
    items = load_inbox()
    items.append(item)
    _atomic_write_json(_INBOX_FILE, items)


def ack_inbox_items(ids: list[str]) -> None:
    keep = [it for it in load_inbox() if it.get("id") not in set(ids)]
    _atomic_write_json(_INBOX_FILE, keep)


# ── Timeline-event queue (extension -> app handoff) ──────────────────────────
# When the extension finishes an autofill it can queue a "mark Applied + note"
# event here. The app polls + applies it to the job's stage timeline, then acks.
# Kept in its OWN python-owned file so we never race the Rust shell's jobs.json.

_TIMELINE_FILE = "timeline.json"


def load_timeline() -> list[dict]:
    raw = _read_json(_TIMELINE_FILE, [])
    return raw if isinstance(raw, list) else []


def add_timeline_event(event: dict) -> None:
    events = load_timeline()
    events.append(event)
    _atomic_write_json(_TIMELINE_FILE, events)


def ack_timeline_events(ids: list[str]) -> None:
    keep = [e for e in load_timeline() if e.get("id") not in set(ids)]
    _atomic_write_json(_TIMELINE_FILE, keep)
