"""Job-capture inbox — the extension queues a captured JD here; the desktop app
polls it, creates the job + kicks off company research, then acks it.

  POST /inbox/job   {company, role, location, url, job_description} -> {ok, id}
  GET  /inbox/jobs                                                  -> [items]
  POST /inbox/ack   {ids:[...]}                                     -> {ok}

Token-guarded (see routes/bridge.py). The app is the single consumer.
"""
from __future__ import annotations

import random
import time

from fastapi import APIRouter, Depends, HTTPException

import runtime_config
import store_reader
from models import InboxAckRequest, JobCaptureRequest, PageExtractRequest, TimelineEventRequest
from routes.bridge import require_token

router = APIRouter(dependencies=[Depends(require_token)])


EXTRACT_PROMPT = """You are reading the text of a single job-posting web page.
Extract these fields:
- company: the hiring company's name.
- role: the job title.
- location: city / region / country, or "Remote" — the best available.
- job_description: the actual posting — summary, responsibilities, requirements,
  qualifications, and benefits. Remove site navigation, cookie/consent banners,
  "related jobs", search UI, and footers. Keep the real content faithfully; do
  NOT invent or summarize away detail.
- employment_type: "Full-time", "Part-time", "Contract", "Internship", or "" if
  not stated.
- work_mode: "Remote", "Hybrid", "On-site", or "" if not stated.
- salary: the pay range exactly as written, compact (e.g. "$180k–$240k/yr"), or
  "" if none is given. Do NOT invent or estimate a figure.
- posted: when it was posted, as written (e.g. "Posted 4 days ago", "2024-05-01"),
  or "" if absent.
- requirements: 3-6 SHORT chips (<=4 words each) capturing the key hard
  requirements — years/skills/stack (e.g. "5+ yrs backend", "Go / Ruby",
  "Distributed systems"). [] if the posting lists none.

Return ONLY a JSON object with exactly these keys: {"company","role","location",
"job_description","employment_type","work_mode","salary","posted","requirements"}.
Use "" (or [] for requirements) for anything genuinely not present."""


@router.post("/job")
async def capture_job(req: JobCaptureRequest):
    item = {
        "id": f"cap-{int(time.time() * 1000)}-{random.randint(100, 999)}",
        "company": req.company.strip(),
        "role": req.role.strip(),
        "location": req.location.strip(),
        "url": req.url.strip(),
        "job_description": req.job_description.strip(),
        "employment_type": req.employment_type.strip(),
        "work_mode": req.work_mode.strip(),
        "salary": req.salary.strip(),
        "posted": req.posted.strip(),
        "requirements": [r.strip() for r in req.requirements if r.strip()][:6],
        "ts": int(time.time()),
    }
    store_reader.add_inbox_item(item)
    return {"ok": True, "id": item["id"]}


@router.get("/jobs")
async def pending_jobs():
    return store_reader.load_inbox()


@router.post("/ack")
async def ack(req: InboxAckRequest):
    store_reader.ack_inbox_items(req.ids)
    return {"ok": True}


# ── Timeline events (autofill -> app stage timeline) ─────────────────────────

@router.post("/timeline")
async def log_timeline(req: TimelineEventRequest):
    """Queue a 'mark Applied + note' event for a job. The desktop app applies it
    to the job's stage timeline on its next poll (see GET /inbox/timeline)."""
    if not req.job_id.strip():
        raise HTTPException(status_code=400, detail="job_id required")
    event = {
        "id": f"tl-{int(time.time() * 1000)}-{random.randint(100, 999)}",
        "job_id": req.job_id.strip(),
        "status": (req.status or "Applied").strip(),
        "note": req.note.strip(),
        "ts": int(time.time()),
    }
    store_reader.add_timeline_event(event)
    return {"ok": True, "id": event["id"]}


@router.get("/timeline")
async def pending_timeline():
    return store_reader.load_timeline()


@router.post("/timeline/ack")
async def ack_timeline(req: InboxAckRequest):
    store_reader.ack_timeline_events(req.ids)
    return {"ok": True}


@router.post("/extract")
async def extract(req: PageExtractRequest):
    """Parse company/role/location/JD out of raw page text with the configured
    LLM, for pages where the client-side heuristics (JSON-LD / DOM) come up
    short."""
    import llm_provider as llm_factory

    cfg = runtime_config.get_llm_config()
    key_err = llm_factory.missing_key_error(cfg)
    if key_err:
        raise HTTPException(status_code=400, detail="AI provider not configured — open InterPrep Settings")
    if not req.page_text.strip():
        raise HTTPException(status_code=400, detail="no page text provided")

    contents = (
        EXTRACT_PROMPT
        + f"\n\n=== URL ===\n{req.url}\n\n=== TITLE ===\n{req.title}\n\n=== PAGE TEXT ===\n{req.page_text[:30000]}"
    )
    try:
        data, _model = await llm_factory.generate_json(cfg, contents, tier="smart", temperature=0.1)
    except Exception as exc:  # noqa: BLE001
        raise HTTPException(status_code=502, detail=f"extraction failed: {exc}")

    reqs = data.get("requirements", [])
    if not isinstance(reqs, list):
        reqs = []
    reqs = [str(r).strip() for r in reqs if str(r).strip()][:6]

    return {
        "company": str(data.get("company", "")).strip(),
        "role": str(data.get("role", "")).strip(),
        "location": str(data.get("location", "")).strip(),
        "job_description": str(data.get("job_description", "")).strip(),
        "employment_type": str(data.get("employment_type", "")).strip(),
        "work_mode": str(data.get("work_mode", "")).strip(),
        "salary": str(data.get("salary", "")).strip(),
        "posted": str(data.get("posted", "")).strip(),
        "requirements": reqs,
    }
