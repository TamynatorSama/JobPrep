"""Application-form autofill for the browser extension.

  POST /autofill/answer-fields  -> answer scraped form fields from resume + JD
  GET  /autofill/memory         -> the saved answer bank
  POST /autofill/remember       -> persist one user-supplied answer

Answer resolution order per field:
  1. Answer bank (qa_memory.json)  — free + instant, set by the user previously.
  2. Gemini                        — one JSON call for everything not in the bank.
  3. needs_user                    — subjective / no resume evidence / no API key.

All endpoints are token-guarded (see routes/bridge.py). The Gemini key is the
in-memory one seeded by the Rust shell — the extension never sends it.
"""
from __future__ import annotations

import json

from fastapi import APIRouter, Depends
from google import genai

import runtime_config
import store_reader
from models import AnswerFieldsRequest, FieldSpec, RememberRequest
from routes.bridge import require_token
# Reuse the resilient model-call + lenient-JSON-parse helpers from the tailor.
from routes.application import _generate_json, _loads_lenient, _response_text

router = APIRouter(dependencies=[Depends(require_token)])


PROMPT = """You are filling out a job application form on behalf of a candidate.
You are given the candidate's TAILORED RESUME for this specific job, the job
context, COMPANY RESEARCH about the employer, and a list of form FIELDS scraped
from the page. For EACH field, decide the best value to enter.

Be HELPFUL, not timid. Fill as many fields as you reasonably can. Prefer a
sensible, low-risk answer over leaving a field for the user. Only fall back to
needs_user for things that genuinely require the user's private decision.

RULES (in priority order)
1. FACTUAL (name, email, phone, current title, years of experience, skills,
   education, links): fill from the resume. High confidence.
2. ADDRESS / LOCATION: fill from the resume's contact info. If the form splits
   the address into separate fields (street / address line 1 / city / state or
   province / ZIP or postal code / country), split the candidate's address
   across them. Fill whatever components the resume supports; only the specific
   sub-fields with no basis (e.g. ZIP when the resume has none) are needs_user.
3. COMPANY-SPECIFIC ("why work here", "what interests you", "what do you know
   about us"): answer from COMPANY RESEARCH when present, grounded + concise.
   needs_user only if no research is available.
4. SALARY / COMPENSATION expectation: RECOMMEND a figure — do NOT punt this to
   the user. Ground it in COMPANY RESEARCH salary data (Glassdoor / Levels /
   Indeed) when present; otherwise estimate the typical market rate for this
   role + seniority + location. Match the field format (single number vs a
   range, hourly vs annual as the label implies). confidence ~0.5 so the user
   reviews it. needs_user only if you genuinely have no basis to estimate.
5. LOW-STAKES / DISCRETIONARY — educated guess, needs_user=FALSE, confidence ~0.5:
     - "How did you hear about us?" -> a plausible source ("LinkedIn",
       "Company website", or the job board in the job context if known).
     - Any select/radio where SEVERAL options are acceptable and picking one is
       harmless -> the most reasonable option.
     - Optional free-text where a short reasonable answer fits.
6. WORK ELIGIBILITY yes/no ("authorized to work in <country>?", "require
   sponsorship?"): infer from the candidate's location / work history when it's
   clearly implied (located in + has work history in that country ->
   authorized=Yes, sponsorship=No). needs_user only when genuinely ambiguous.
7. VOLUNTARY SELF-ID / EEO (gender, race, veteran, disability): if an option
   like "Decline to self-identify" / "Prefer not to say" / "I don't wish to
   answer" exists, choose it (confidence ~0.5). Otherwise needs_user.
8. NEEDS_USER (value "") — reserve for: exact available start date and anything
   legally/financially consequential you truly cannot infer, plus any
   select/radio where NO option is acceptable.

FORMAT CONSTRAINTS
- type "select"/"radio": value MUST be EXACTLY one of the provided options
  (copy verbatim). If none fit, needs_user=true.
- type "checkbox": value is "true" or "false".
- Set confidence honestly: ~0.9 hard facts, ~0.5 educated guesses. Never
  fabricate employers, degrees, dates, certifications, or specific numbers.

Return ONLY a JSON array (no prose, no code fences), one object per field, in the
SAME ORDER as the input, each shaped:
{ "key": "<the field key>", "value": "<string>", "confidence": 0.0-1.0,
  "needs_user": true|false, "reason": "<short why, <=12 words>" }
"""


@router.post("/answer-fields")
async def answer_fields(req: AnswerFieldsRequest):
    # ── Resolve job context (store first, then request overrides) ────────────
    company, role, location, jd = req.company, req.role, "", req.job_description
    tailored, research = "", ""
    if req.job_id:
        job = store_reader.get_raw_job(req.job_id)
        if job:
            company = company or job.get("company", "")
            role = role or job.get("role", "")
            location = job.get("location", "")
            jd = jd or (job.get("jobDescription") or "")
            tailored = job.get("tailoredResume") or ""
            research = store_reader.research_text(job)

    # ── Evidence pool ────────────────────────────────────────────────────────
    # The tailored resume drives CONTENT, but its contact line is trimmed to
    # "city, country" — so always include the master resume(s) too for full
    # contact details (street address, phone, etc.). resume_ids empty = all.
    evidence_parts = []
    if tailored:
        evidence_parts.append(f"=== TAILORED RESUME (for this job) ===\n{tailored}")
    for r in store_reader.resumes_text(req.resume_ids):
        if r.get("text"):
            evidence_parts.append(f"=== MASTER RESUME: {r['name']} ===\n{r['text']}")
    evidence = "\n\n".join(evidence_parts)

    memory = store_reader.load_memory()

    # ── Split: bank hits vs fields that need the model ───────────────────────
    answers: dict[str, dict] = {}
    to_model: list[FieldSpec] = []
    for f in req.fields:
        key = store_reader.normalize_label(f.label)
        hit = memory.get(key)
        if hit and hit.get("value"):
            answers[f.key] = {
                "key": f.key, "value": hit["value"], "confidence": 1.0,
                "needs_user": False, "reason": "from saved answers", "source": "memory",
            }
        else:
            to_model.append(f)

    # ── Gemini for the rest ──────────────────────────────────────────────────
    api_key = runtime_config.get_api_key()
    have_context = bool(evidence or research)
    if to_model and api_key and have_context:
        try:
            model_answers = await _ask_model(
                api_key, company, role, location, jd, evidence, research, to_model
            )
            for f in to_model:
                answers[f.key] = model_answers.get(f.key, _unanswered(f, "model returned no answer"))
        except Exception as exc:  # noqa: BLE001 — degrade to needs_user, never 500
            print(f"[autofill] model call failed: {exc!r}", flush=True)
            for f in to_model:
                answers[f.key] = _unanswered(f, "model error — answer manually")
    else:
        reason = (
            "no resume/research for this job" if not have_context
            else "Gemini key not set — open InterPrep Settings" if not api_key
            else "answer manually"
        )
        for f in to_model:
            answers[f.key] = _unanswered(f, reason)

    # Preserve the request order.
    return {"answers": [answers[f.key] for f in req.fields]}


@router.get("/memory")
async def memory():
    return store_reader.load_memory()


@router.post("/remember")
async def remember(req: RememberRequest):
    store_reader.remember(req.label, req.value)
    return {"ok": True}


# ── helpers ──────────────────────────────────────────────────────────────────

def _unanswered(f: FieldSpec, reason: str) -> dict:
    return {"key": f.key, "value": "", "confidence": 0.0,
            "needs_user": True, "reason": reason, "source": "needs_user"}


def _field_line(f: FieldSpec) -> str:
    opts = f" | options: {f.options}" if f.options else ""
    return f'- key="{f.key}" type={f.type} label="{f.label}"{opts}'


async def _ask_model(api_key, company, role, location, jd, evidence, research, fields: list[FieldSpec]) -> dict:
    loc = f"\nLocation: {location}" if location else ""
    fields_block = "\n".join(_field_line(f) for f in fields)
    research_block = (
        f"=== COMPANY RESEARCH ===\n{research}\n\n" if research
        else "=== COMPANY RESEARCH ===\n(none available)\n\n"
    )
    contents = (
        PROMPT
        + "\n\n=== JOB CONTEXT ===\n"
        + f"Company: {company}\nRole: {role}{loc}\n\n"
        + f"=== JOB DESCRIPTION ===\n{jd}\n\n"
        + f"=== CANDIDATE EVIDENCE ===\n{evidence or '(no resume on file)'}\n\n"
        + research_block
        + f"=== FIELDS TO ANSWER ({len(fields)}) ===\n{fields_block}\n"
    )
    client = genai.Client(api_key=api_key)
    resp, _model = await _generate_json(client, contents, temperature=0.2)
    parsed = _loads_lenient_array(_response_text(resp))
    out: dict[str, dict] = {}
    valid_keys = {f.key for f in fields}
    for item in parsed:
        if not isinstance(item, dict):
            continue
        key = item.get("key")
        if key not in valid_keys:
            continue
        out[key] = {
            "key": key,
            "value": str(item.get("value", "")),
            "confidence": float(item.get("confidence", 0.0) or 0.0),
            "needs_user": bool(item.get("needs_user", False)),
            "reason": str(item.get("reason", "")),
            "source": "model",
        }
    return out


def _loads_lenient_array(text: str) -> list:
    """Parse the first JSON array out of a model response, tolerating fences /
    leading prose / trailing junk (mirrors application._loads_lenient but for a
    top-level array)."""
    import re
    t = text.strip()
    t = re.sub(r"^```(?:json)?\s*", "", t, flags=re.MULTILINE)
    t = re.sub(r"\s*```$", "", t, flags=re.MULTILINE).strip()
    start = t.find("[")
    if start == -1:
        # Some models wrap the array in {"answers": [...]}; fall back to that.
        obj = _loads_lenient(text)
        if isinstance(obj, dict):
            for v in obj.values():
                if isinstance(v, list):
                    return v
        return []
    arr, _ = json.JSONDecoder().raw_decode(t[start:])
    return arr if isinstance(arr, list) else []
