"""Read-only store endpoints for the browser extension.

  GET /store/jobs      -> [{id, company, role, location, url, jobDescription, ...}]
  GET /store/resumes   -> [{id, name, text}]

Serves the same data the desktop app holds, read straight off the JSON files in
``%LOCALAPPDATA%\\InterPrep``. Token-guarded — see routes/bridge.py.
"""
from __future__ import annotations

import base64
import re

from fastapi import APIRouter, Depends, HTTPException

import store_reader
from routes.bridge import require_token

router = APIRouter(dependencies=[Depends(require_token)])


def sanitize_name(s: str) -> str:
    """Clean a name/company for use in a filename: strip illegal chars, drop the
    "logo" scrape artifact, and collapse duplicated text like
    'Armada Recovery logo Armada Recovery' -> 'Armada Recovery'."""
    s = re.sub(r'[<>:"/\\|?*\x00-\x1f]', " ", s or "")
    s = re.sub(r"\blogo\b", " ", s, flags=re.I)
    s = re.sub(r"\s+", " ", s).strip()
    words = s.split(" ") if s else []
    # "X Y X Y" -> "X Y"
    n = len(words)
    if n >= 2 and n % 2 == 0 and [w.lower() for w in words[:n // 2]] == [w.lower() for w in words[n // 2:]]:
        words = words[:n // 2]
    # collapse immediate duplicate words ("Armada Armada" -> "Armada")
    dedup = []
    for w in words:
        if not dedup or dedup[-1].lower() != w.lower():
            dedup.append(w)
    return " ".join(dedup).strip()


@router.get("/jobs")
async def jobs():
    return store_reader.load_jobs()


@router.get("/resumes")
async def resumes():
    return store_reader.load_resumes()


@router.get("/resume-file/{job_id}")
async def resume_file(job_id: str):
    """The job's tailored resume as a base64 PDF for the extension to attach to
    file-upload fields. Built from the tailored-resume text. Filename is
    '<FullName>_<Company>.pdf' (both sanitized)."""
    job = store_reader.get_raw_job(job_id)
    if not job:
        raise HTTPException(status_code=404, detail="job not found")

    tailored = (job.get("tailoredResume") or "").strip()
    if not tailored:
        raise HTTPException(status_code=404, detail="no tailored resume for this job")

    from resume_pdf import build_pdf, candidate_name
    data = build_pdf(tailored)

    name = sanitize_name(candidate_name(tailored))
    company = sanitize_name(job.get("company") or "") or "Company"
    stem = f"{name}_{company}" if name else company
    return {
        "filename": f"{stem}.pdf",
        "mime": "application/pdf",
        "b64": base64.b64encode(data).decode("ascii"),
    }
