"""
Interview-cheatsheet route.

POST /cheatsheet/build — aggregate a job's JD, resume, company-research dossier,
and prior conversations into a structured cheatsheet the copilot's Cheatsheet tab
renders (STAR stories, key facts, questions to ask) plus a maintained markdown
doc the app persists as the job's living cheatsheet file.

Non-streaming: returns one JSON object. The LLM produces the structured fields;
the markdown is assembled deterministically from them so the saved file always
matches what the UI shows (no second parse to drift).
"""
import time

from fastapi import APIRouter

import llm_provider as llm_factory
from models import CheatsheetRequest


router = APIRouter()

# Cap each context block so a long dossier / transcript history can't blow the
# context window. Generous — these are trimmed, not summarized, before the call.
_RESUME_CAP = 5000
_RESEARCH_CAP = 7000
_DOC_CAP = 2500
_DOCS_TOTAL_CAP = 14000
_PREV_CAP = 6000

_PROMPT = """You are an elite interview coach preparing a candidate for a specific role.
Build a concise, high-signal interview CHEATSHEET from the material below.

Return ONLY a JSON object (no prose, no code fences) with exactly this shape:
{{
  "summary": "one or two sentences: the candidate's sharpest positioning for THIS role",
  "stories": [
    {{
      "title": "short STAR story title",
      "tag": "one of: Ownership | Debugging | Behavioral | Leadership | Conflict | Impact | Technical",
      "metric": "the headline result, with numbers when available",
      "beats": ["3-5 short bullet beats: situation, action, result — each a tight phrase"]
    }}
  ],
  "facts": [
    {{ "k": "short label", "v": "the fact — a metric, a 'why this company' angle, a strength" }}
  ],
  "questions": [
    {{ "q": "a sharp question for the candidate to ASK the interviewer", "why": "what it signals / why it lands" }}
  ]
}}

Rules:
- Ground EVERYTHING in the provided material. Prefer real numbers and specifics from the resume and conversations. Do not invent employers, metrics, or facts.
- STAR stories: mine them from the resume and any mock-interview answers. 3-6 stories, the strongest first.
- Facts: 4-8 items — concrete metrics, "why this company" angles tied to the research, and the candidate's differentiators.
- Questions to ask: 4-6, tailored to the company/role and informed by the research (e.g. reference a real product, challenge, or value).
- If a previous cheatsheet is provided, REFINE it: keep what's still strong, fold in new signal from the latest conversations, drop the weak.
- Keep every string tight and spoken-answer friendly. No markdown inside the JSON values.

=== ROLE ===
Company: {company}
Role: {role}
Location: {location}

=== JOB DESCRIPTION ===
{job_description}

=== CANDIDATE RESUME ===
{resume}

=== COMPANY RESEARCH DOSSIER ===
{company_research}

=== CONVERSATIONS (coach chats / mock interviews) ===
{documents}

=== PREVIOUS CHEATSHEET (refine this if present) ===
{previous_markdown}
"""


def _clip(text: str, cap: int) -> str:
    text = (text or "").strip()
    return text if len(text) <= cap else text[:cap] + "\n…(truncated)"


def _format_documents(docs) -> str:
    if not docs:
        return "(none)"
    out, total = [], 0
    for d in docs:
        body = _clip(d.text, _DOC_CAP)
        if not body:
            continue
        block = f"[{d.source}]\n{body}"
        total += len(block)
        out.append(block)
        if total >= _DOCS_TOTAL_CAP:
            break
    return "\n\n".join(out) if out else "(none)"


def _coerce_list(value):
    return value if isinstance(value, list) else []


def _build_markdown(company: str, role: str, data: dict) -> str:
    """Assemble the persisted markdown deterministically from the structured
    fields so the saved file always matches the rendered tab."""
    lines = [f"# Interview Cheatsheet — {role} @ {company}", ""]
    summary = (data.get("summary") or "").strip()
    if summary:
        lines += [summary, ""]

    lines.append("## STAR Stories")
    stories = _coerce_list(data.get("stories"))
    if not stories:
        lines.append("_None yet._")
    for s in stories:
        title = (s.get("title") or "Untitled").strip()
        tag = (s.get("tag") or "").strip()
        metric = (s.get("metric") or "").strip()
        head = f"### {title}"
        if tag:
            head += f"  · _{tag}_"
        lines.append(head)
        if metric:
            lines.append(f"**Result:** {metric}")
        for beat in _coerce_list(s.get("beats")):
            if str(beat).strip():
                lines.append(f"- {str(beat).strip()}")
        lines.append("")

    lines.append("## Key Facts")
    facts = _coerce_list(data.get("facts"))
    if not facts:
        lines.append("_None yet._")
    for f in facts:
        k = (f.get("k") or "").strip()
        v = (f.get("v") or "").strip()
        if k or v:
            lines.append(f"- **{k}:** {v}" if k else f"- {v}")
    lines.append("")

    lines.append("## Questions to Ask")
    questions = _coerce_list(data.get("questions"))
    if not questions:
        lines.append("_None yet._")
    for q in questions:
        qt = (q.get("q") or "").strip()
        why = (q.get("why") or "").strip()
        if qt:
            lines.append(f"- {qt}" + (f"  \n  _Why: {why}_" if why else ""))
    lines.append("")

    return "\n".join(lines).strip() + "\n"


@router.post("/build")
async def build_cheatsheet(req: CheatsheetRequest):
    prompt = _PROMPT.format(
        company=req.company or "(unknown)",
        role=req.role or "(unknown)",
        location=req.location or "(unspecified)",
        job_description=_clip(req.job_description, 4000) or "(none provided)",
        resume=_clip(req.resume, _RESUME_CAP) or "(none provided)",
        company_research=_clip(req.company_research, _RESEARCH_CAP) or "(none yet)",
        documents=_format_documents(req.documents),
        previous_markdown=_clip(req.previous_markdown, _PREV_CAP) or "(none — build fresh)",
    )

    try:
        data, model = await llm_factory.generate_json(req.llm, prompt, tier="smart", temperature=0.35)
    except Exception as exc:  # noqa: BLE001 — surface any provider/parse failure to the client
        return {"error": f"cheatsheet generation failed: {exc}"}

    if not isinstance(data, dict):
        return {"error": "model did not return a cheatsheet object"}

    stories = _coerce_list(data.get("stories"))
    facts = _coerce_list(data.get("facts"))
    questions = _coerce_list(data.get("questions"))
    summary = (data.get("summary") or "").strip()
    markdown = _build_markdown(req.company, req.role, data)

    return {
        "summary": summary,
        "stories": stories,
        "facts": facts,
        "questions": questions,
        "markdown": markdown,
        "model": model,
        "updatedAt": int(time.time() * 1000),
    }
