"""
Interview-cheatsheet route.

POST /cheatsheet/build — aggregate a job's JD, resume, company-research dossier,
and prior conversations into a structured cheatsheet the copilot's Cheatsheet tab
renders (STAR stories, key facts, questions to ask) plus a maintained markdown
doc the app persists as the job's living cheatsheet file.

Non-streaming: returns one JSON object. The LLM produces the structured fields;
the markdown is assembled deterministically from them so the saved file always
matches what the UI shows (no second parse to drift).

Incremental builds (the automatic one after a mock interview): the response's
`seen` map records how much of each conversation the sheet has read; the next
request with `incremental` + `previous` sends the model only the previous sheet
and the conversation text added since — no JD, resume or research again, and
no call at all when nothing is new. A manual refresh rebuilds from everything.
"""
import json
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
_DELTA_DOC_CAP = 8000

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


_DELTA_PROMPT = """You are an elite interview coach keeping a candidate's interview CHEATSHEET up to date.
Below is the current cheatsheet (JSON) and the conversation text added since it was built
(coach chats / mock interviews for {role} at {company}). Fold the new signal in: add or
sharpen STAR stories the candidate actually told, update facts with new specifics, adjust
questions to ask if the conversation suggests better ones. Keep everything that is still
strong; drop only what the new material shows is weak or wrong.

Return ONLY the full updated JSON object (no prose, no code fences), same shape as the
current one: {{"summary", "stories": [{{"title","tag","metric","beats"}}], "facts": [{{"k","v"}}],
"questions": [{{"q","why"}}]}}. 3-6 stories, 4-8 facts, 4-6 questions.

Rules:
- Ground EVERYTHING in the cheatsheet or the new conversation text. Do not invent employers,
  metrics, or facts.
- In a mock interview the "Assistant" is a SIMULATED interviewer: what it asked shows what to
  prepare for, but its claims about the company, team or process are not facts — only the
  candidate's own words are evidence about the candidate.
- Keep every string tight and spoken-answer friendly. No markdown inside the JSON values.

=== CURRENT CHEATSHEET ===
{previous}

=== NEW CONVERSATION TEXT ===
{documents}
"""


def _doc_key(d) -> str:
    return d.id or d.source


def _seen(docs) -> dict:
    """How much of each conversation this build has read (chars), stored on the
    cheatsheet so the next incremental build sends only what's new."""
    return {_doc_key(d): len(d.text or "") for d in docs}


def _deltas(docs, seen: dict) -> list:
    """Conversation text added since `seen` (threads only ever grow)."""
    out = []
    for d in docs:
        before = int(seen.get(_doc_key(d), 0) or 0)
        text = d.text or ""
        if len(text) > before:
            out.append(type(d)(id=d.id, source=d.source + (" (continued)" if before else ""),
                               text=text[before:]))
    return out


def _clip(text: str, cap: int) -> str:
    text = (text or "").strip()
    return text if len(text) <= cap else text[:cap] + "\n…(truncated)"


def _format_documents(docs, doc_cap: int = _DOC_CAP) -> str:
    if not docs:
        return "(none)"
    out, total = [], 0
    for d in docs:
        body = _clip(d.text, doc_cap)
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
    prev = req.previous if isinstance(req.previous, dict) else {}
    seen = prev.get("seen")
    if req.incremental and _coerce_list(prev.get("stories")) and isinstance(seen, dict):
        # The automatic rebuild after a mock interview: the previous sheet +
        # only the conversation text added since it was built. JD, resume and
        # research are already distilled into the sheet. A manual refresh
        # (incremental=false) still rebuilds from everything.
        new_docs = _deltas(req.documents, seen)
        if not new_docs:
            return {**{k: prev.get(k) for k in ("summary", "stories", "facts", "questions",
                                               "markdown", "updatedAt")},
                    "seen": _seen(req.documents), "model": ""}
        current = {k: prev.get(k) for k in ("summary", "stories", "facts", "questions")}
        prompt = _DELTA_PROMPT.format(
            company=req.company or "(unknown)", role=req.role or "(unknown)",
            previous=json.dumps(current, ensure_ascii=False),
            # New text is the whole point here — let a mock interview through
            # in full rather than the full build's 2,500-char slice per thread.
            documents=_format_documents(new_docs, doc_cap=_DELTA_DOC_CAP),
        )
    else:
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
        "seen": _seen(req.documents),
    }
