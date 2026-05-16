"""
Application-tailoring route.

POST /application/tailor-resume — given a job (company, role, JD) and the user's
master resumes, ask Gemini 2.5 Pro to:
  1. pick the closest-matching master resume
  2. produce a tailored, ATS-friendly resume (returned as a .docx)
  3. produce a cover letter (streamed as tokens for the chat bubble)
  4. produce a scorecard (verbatim-match %, role-title alignment, etc.)

The handler is SSE and emits these event types:
  stage          banner (rendered into the logs sidebar by the Rust client)
  token          cover-letter prose (rendered into the chat bubble)
  scorecard      structured JSON (Rust renders as a card)
  resume_docx    base64-encoded .docx bytes (Rust saves to disk)
  done | error
"""
import asyncio
import base64
import io
import json
import re

from fastapi import APIRouter
from sse_starlette.sse import EventSourceResponse

from google import genai
from google.genai import types as genai_types
from docx import Document
from docx.shared import Pt

from models import ApplicationRequest, KnockoutRequest


router = APIRouter()

# ── Master prompt (verbatim from the user's spec) ────────────────────────────
MASTER_PROMPT = """\
Persona and Role:

* Act as a dual-expert consultant embodying the roles of a Senior Technical \
Recruiter, an ATS Logic Engineer, and a Strategic Career Hacker.

* Your objective is to engineer an application that mathematically maximizes \
the probability of an interview by exploiting the mechanics of Applicant \
Tracking Systems (ATS) like Greenhouse, Lever, and Workable, and the psychology \
of recruiters.

* You operate based on 'Verbatim Match', 'Hire/ No Hire', and 'Structure \
Mirroring' methodologies, treating a resume as a dedicated proof-of-match \
document for a specific Job Description (JD).


Operational Workflow:


PHASE 1: Semantic Extraction & Strategy Setup

1. Request Input: One or more master resumes are provided below; the JD is \
provided too. Pick the closest match.

2. The Verbatim Audit: Scan the JD for specific technical phrases. Identify \
exact strings (e.g., 'manipulate large datasets' instead of 'big data handling').

3. Structure Analysis: Identify headers in the JD responsibilities (e.g., \
'Data Strategy') to use as CV sub-headers later.

4. Values Detection: Flag terms from the JD's 'Values' or 'Culture' section \
for alignment.


PHASE 2: System-Beater Resume Optimization

Generate a revised, text-only resume using these rules:

* Benchmark: Utilize standard patterns for Data Analyst, Data Scientist, or AI \
Engineer roles as benchmarks where applicable.

* Rule 1 (Role Title Trick): Match the candidate's Resume Job Title to the JD \
title exactly, provided responsibilities align. Do not falsely upgrade seniority.

* Rule 2 (Structure Mirroring): Categorize bullet points under sub-headers that \
mirror the JD's section headers.

* Rule 3 (Verbatim Keyword Injection): Integrate identified exact phrases \
naturally into bullet points.

* Rule 4 (Values Alignment): Create a section titled 'Values Alignment' linking \
traits to company keywords.

* Rule 5 (Quantification): Apply the Google XYZ formula (Accomplished [X] as \
measured by [Y], by doing [Z]). Every claim must include a number (%, $, time). \
Use estimates if necessary.

* Rule 6 (Proactive Logistics): Add an 'Additional Information' section \
addressing location and visa status when known.

* Formatting: Single-column, ATS-friendly. No tables, or columns. No citation \
markers like '[cite start]'.


PHASE 3: Hook & Match Cover Letter

Draft a letter using this 5-paragraph formula:

1. The Hook: Open with enthusiasm for the specific company mission/product.

2. Experience Story: Provide a condensed, quantified STAR format example.

3. Why This Role: Mention specific excitement about JD responsibilities.

4. Close: Confirm location/availability and request a discussion.

* Tone: Human, natural, using contractions. Avoid 'I am writing to express my \
interest.'


PHASE 4: Final Output & Analysis

1. Output: Provide rewritten Resume and Cover Letter.

2. Scorecard: Technical Match: List skills using JD terminology. Provide an \
objective 'Hire' or 'No Hire' recommendation based on fit.

3. Also produce: 'Verbatim Match Score' (0-100%), 'Role Title Alignment' \
(Yes/No), and 'Quantification Check' (Pass/Needs Work).


Operational Guardrails:

* No Hallucination: Do not invent work history. If a required skill isn't in \
the resumes, omit it rather than fabricate it.

* Formatting: .docx compatible. Do not include metadata or citation text.

* Tone: Professional, authoritative, and tactical coach.
"""

# Output schema the LLM must follow. Embedded into the user prompt because
# google-genai's `response_schema` rejects long descriptions.
OUTPUT_SHAPE = """\
Return ONLY a single JSON object (no markdown fences, no leading prose) with \
this exact shape:
{
  "selected_resume_label": "<label of the master resume you used as the base>",
  "resume_text": "<the full tailored resume as PLAIN TEXT.\\nUse these conventions for the docx renderer:\\n  Line 1 = candidate name\\n  Line 2 = single-line contact info (email | phone | location | linkedin)\\n  Section headers: '## Section Name'\\n  Bullets: '- bullet text'\\n  Paragraphs: plain lines.\\nNo emoji, no tables, no columns. ATS-safe.>",
  "cover_letter": "<the full cover letter, ready to copy/paste. Plain prose with paragraph breaks.>",
  "scorecard": {
    "verbatim_match_score": 0-100,
    "role_title_alignment": "Yes" | "No",
    "quantification_check": "Pass" | "Needs Work",
    "hire_recommendation": "Hire" | "No Hire",
    "skills_matched": ["skill1", "skill2", "..."]   // SHORT keyword skills only: 1-3 words each, max 25 chars. Examples: "Python", "Distributed systems", "RLHF", "AWS Lambda", "SQL". NEVER full phrases like "develop, test, and maintain software" — break those into individual skill keywords. 6-10 skills total.
  }
}"""


@router.post("/tailor-resume")
async def tailor_resume(req: ApplicationRequest):
    async def generate():
        if not req.api_key:
            yield _evt("error", "No Gemini API key set. Open Settings → API Keys.")
            return
        if not req.master_resumes:
            yield _evt("error", "No master resumes provided. Add one in Settings → Resume.")
            return
        if not req.company.strip() or not req.role.strip():
            yield _evt("error", "Company and role are required.")
            return

        yield _evt("stage", "\n\n---\n📋 **Analyzing JD and picking the best master resume…**\n\n")
        await asyncio.sleep(0)

        prompt = _build_prompt(req)

        try:
            client = genai.Client(api_key=req.api_key)

            yield _evt("stage", "\n📝 **Tailoring resume and drafting cover letter (Gemini 2.5 Pro)…**\n\n")
            await asyncio.sleep(0)

            # Single call returning structured JSON. Pro handles this prompt
            # well; we don't need to stream the model output token-by-token
            # because the JSON would be unparseable mid-stream.
            response = await client.aio.models.generate_content(
                model="gemini-2.5-pro",
                contents=prompt,
                config=genai_types.GenerateContentConfig(
                    response_mime_type="application/json",
                    temperature=0.4,
                ),
            )

            raw = _response_text(response)
            try:
                data = json.loads(_strip_fences(raw))
            except json.JSONDecodeError as exc:
                yield _evt("error", f"Model returned non-JSON output: {exc}")
                return

            resume_text  = (data.get("resume_text")  or "").strip()
            cover_letter = (data.get("cover_letter") or "").strip()
            scorecard    = data.get("scorecard")    or {}
            chosen_label = data.get("selected_resume_label") or "(unspecified)"

            if not resume_text or not cover_letter:
                yield _evt("error", "Model output was missing the resume or cover letter.")
                return

            yield _evt("stage", f"\n✨ **Using master resume:** _{chosen_label}_\n\n")
            await asyncio.sleep(0)

            # ── Stream the cover letter as tokens (word-by-word) so the chat
            #    bubble fills in incrementally — same UX as the chat route.
            yield _evt("stage", "\n💌 **Cover Letter**\n\n")
            for word in re.split(r"(\s+)", cover_letter):
                if word:
                    yield _evt("token", word)
                    await asyncio.sleep(0.005)
            yield _evt("token", "\n\n")

            # ── Build the .docx and send it back as base64 ──────────────────
            yield _evt("stage", "\n📄 **Building tailored resume.docx…**\n\n")
            await asyncio.sleep(0)
            docx_bytes = _build_docx(resume_text)
            yield _evt(
                "resume_docx",
                base64.b64encode(docx_bytes).decode("ascii"),
            )

            # ── Scorecard ───────────────────────────────────────────────────
            yield {"data": json.dumps({"type": "scorecard", "content": scorecard})}
            await asyncio.sleep(0)

            # ── Pass the tailored resume text back so the client can hand
            #    it to the company-research step (used as background context).
            yield {"data": json.dumps({"type": "tailored_resume", "content": resume_text})}
            await asyncio.sleep(0)

            yield _evt("done", "")

        except Exception as exc:
            yield _evt("error", f"Application tailor error: {exc}")

    return EventSourceResponse(generate())


# ── Knockout-question screening ──────────────────────────────────────────────

KNOCKOUT_PROMPT = """\
You are a senior technical recruiter at the target company running a 15–20
minute KNOCKOUT phone screen. The explicit purpose of this call is to
DISQUALIFY weak candidates before they waste the hiring manager's time.
You've read the JD top-to-bottom and the candidate's tailored resume. You
are tactical, time-pressured, and you do not tolerate fluff.

# What to produce

Predict the EXACT 6–8 questions you would actually ask in this screen, in
the order you'd ask them. Your output is a written prep dossier the
candidate will study before the real call.

# Question mix (this is the spine — pick from these buckets so the screen feels real, not a checklist)

(A) LOGISTICS — 1–2 questions. Location/relocation, work authorization /
visa, salary expectations vs the JD's posted band (or current market for
this title), notice period, remote/hybrid commitment if specified in the JD.

(B) MUST-HAVE TECHNICAL — 2–3 questions. Identify the 2–3 hardest technical
requirements in the JD. For each, write a question whose answer immediately
reveals whether the candidate ACTUALLY has the skill versus just listed the
keyword.
  - For "experience with distributed systems": "Walk me through the last
    time you debugged a distributed-system issue in production. What was
    the failure mode and how did you identify it?"
  - For "SQL proficiency": "Describe a query you wrote in the last month
    that was painful to optimize. What was the bottleneck?"
  - For "ML model deployment": "Tell me about a model you actually deployed.
    What monitoring did you set up, and what alerted you to model drift?"
  ABSOLUTELY DO NOT ask textbook trivia ("what is normalization?",
  "explain TCP/IP"). Every technical question MUST ask for a real story or
  a concrete past experience.

(C) BEHAVIORAL RED-FLAG CHECK — 1 question. Scan the resume for the actual
red flag (short tenure, employment gap, lateral move, industry switch, no
recent achievements, suspicious title progression). Ask the question that
surfaces whether it's a real problem.
  Example: "I see you were at [X] for 8 months between [Y] and [Z] — walk
  me through what happened."

(D) SCENARIO / ROLE-PLAY — 1 question. Give a realistic on-the-job
scenario the candidate would hit in this exact role and ask how they'd
handle it. The scenario MUST be specific to what THIS company does, not
generic. Use the company's domain.
  Example for a fintech SWE: "Our primary payment processor returns 500s
  during peak Friday volume. You're the on-call engineer. Walk me through
  your first 10 minutes."
  Example for an ML engineer at a recsys company: "Your A/B test shows the
  new ranking model has 4% higher engagement but 12% more 'show fewer like
  this' clicks. What do you do?"

(E) MOTIVATION & CULTURE FIT — 1 question. Why THIS company specifically
(not "I want to grow my skills"), grounded in something concrete: a product,
recent announcement, mission element. One culture-fit probe if the company
has known cultural values worth testing.

# Output format

For each question, output EXACTLY this structure:

## Question N — [TYPE in caps, e.g. TECHNICAL or SCENARIO]

> [The actual question verbatim, as if a recruiter is speaking. One question. Conversational tone.]

*Why they're asking:* [One sentence — the specific red flag this surfaces.]

**Your answer:** [Concrete answer drawn from the resume. Name the specific project, the specific tech, the specific metric. 3–6 sentences. If the resume doesn't fully support an answer, lead with "**GAP — **" and write an honest one-sentence frame that does not lie.]

**Likely follow-up:** [The probing question the recruiter will ask next based on your answer. Plus a one-sentence handle for it.]

**What NOT to say:** [The vague / buzzword / generic answer that would fail this question.]

After the questions, output a final section:

# Verdict

**Predicted outcome:** Pass / Borderline / Fail
**The one weakness:** [The single biggest hole between this resume and this JD. One sentence, brutally honest.]
**Tonight's prep:** [The ONE thing to rehearse before the actual call. Specific, actionable, under 30 minutes of work.]

# Absolute rules

- Never invent resume content. Every "Your answer" must cite something that's actually in the resume.
- Never use stock interview questions ("tell me about yourself", "what's your greatest weakness", "where do you see yourself in 5 years"). The candidate has heard them all.
- Every question must be specific to THIS candidate + THIS role + THIS company.
- The order matters — open with logistics, build through technical, end on motivation.
- Single questions only. No multi-part. No bullet-list questions.
- Stream the output as markdown — H2 per question, H1 only for the final Verdict section.
"""


@router.post("/knockout-screen")
async def knockout_screen(req: KnockoutRequest):
    async def generate():
        if not req.api_key:
            yield _evt("error", "No Gemini API key set. Open Settings → API Keys.")
            return
        if not req.tailored_resume.strip():
            yield _evt(
                "error",
                "Run the Application Prep step first — the knockout screen "
                "needs the tailored resume as input.",
            )
            return
        if not req.company.strip() or not req.role.strip():
            yield _evt("error", "Company and role are required.")
            return

        yield _evt("stage", "\n\n---\n🎯 **Simulating recruiter knockout screen (Gemini 2.5 Pro)…**\n\n")
        await asyncio.sleep(0)

        loc = f"\nLocation: {req.location}" if req.location else ""
        contents = (
            KNOCKOUT_PROMPT
            + "\n\n=== JOB ===\n"
            + f"Company: {req.company}\nRole: {req.role}{loc}\n\n"
            + f"=== JOB DESCRIPTION ===\n{req.job_description}\n\n"
            + f"=== CANDIDATE'S TAILORED RESUME ===\n{req.tailored_resume}\n"
        )

        try:
            client = genai.Client(api_key=req.api_key)

            # True streaming — these questions form a narrative the user wants
            # to watch render in the chat bubble.
            async for chunk in await client.aio.models.generate_content_stream(
                model="gemini-2.5-pro",
                contents=contents,
                config=genai_types.GenerateContentConfig(temperature=0.5),
            ):
                text = _response_text(chunk)
                if text:
                    yield _evt("token", text)
                    await asyncio.sleep(0)

            yield _evt("done", "")

        except Exception as exc:
            yield _evt("error", f"Knockout screen error: {exc}")

    return EventSourceResponse(generate())


# ── helpers ──────────────────────────────────────────────────────────────────

def _evt(etype: str, content: str) -> dict:
    return {"data": json.dumps({"type": etype, "content": content})}


def _strip_fences(text: str) -> str:
    t = text.strip()
    t = re.sub(r"^```(?:json)?\s*", "", t, flags=re.MULTILINE)
    t = re.sub(r"\s*```$", "", t, flags=re.MULTILINE)
    return t.strip()


def _response_text(response) -> str:
    if hasattr(response, "text") and response.text:
        return response.text
    try:
        parts = response.candidates[0].content.parts
        return "".join(getattr(p, "text", "") for p in parts)
    except Exception:
        return str(response)


def _build_prompt(req: ApplicationRequest) -> str:
    resumes_block = "\n\n".join(
        f"=== MASTER RESUME: {r.name} ===\n{r.text}"
        for r in req.master_resumes
    )
    loc = f"\nLocation: {req.location}" if req.location else ""
    return (
        MASTER_PROMPT
        + "\n\n=== JOB CONTEXT ===\n"
        + f"Company: {req.company}\n"
        + f"Role: {req.role}{loc}\n\n"
        + f"=== JOB DESCRIPTION ===\n{req.job_description}\n\n"
        + f"=== AVAILABLE MASTER RESUMES ({len(req.master_resumes)}) ===\n"
        + resumes_block
        + "\n\n=== TASK ===\n"
        + OUTPUT_SHAPE
    )


def _build_docx(resume_text: str) -> bytes:
    """Render the LLM's plain-text resume into a clean, ATS-friendly .docx.

    Conventions honored:
        First line       → candidate name (heading)
        Second line      → contact info (italic-ish, smaller)
        '## …'           → section heading
        '- …'            → bullet
        '**…**' bare line → bold paragraph
        '' (blank)       → spacer
        anything else    → paragraph
    """
    doc = Document()

    # Single-column, default style. Calibri 11 is the most ATS-tolerant
    # default in 2026 (Word's stock font).
    normal = doc.styles["Normal"]
    normal.font.name = "Calibri"
    normal.font.size = Pt(11)

    lines = [ln.rstrip() for ln in resume_text.split("\n")]
    saw_name = False

    for raw in lines:
        line = raw.strip()
        if not line:
            doc.add_paragraph("")
            continue

        if not saw_name:
            heading = doc.add_paragraph()
            run = heading.add_run(line)
            run.bold = True
            run.font.size = Pt(18)
            saw_name = True
            continue

        if line.startswith("## "):
            sec = doc.add_paragraph()
            run = sec.add_run(line[3:].strip().upper())
            run.bold = True
            run.font.size = Pt(12)
            continue

        if line.startswith("# "):
            sec = doc.add_paragraph()
            run = sec.add_run(line[2:].strip())
            run.bold = True
            run.font.size = Pt(14)
            continue

        if line.startswith("- ") or line.startswith("* "):
            doc.add_paragraph(line[2:].strip(), style="List Bullet")
            continue

        # Bare bold line e.g. **Education** as a sub-header
        if line.startswith("**") and line.endswith("**") and len(line) > 4:
            p = doc.add_paragraph()
            r = p.add_run(line[2:-2])
            r.bold = True
            continue

        # Inline bold in regular paragraphs: split on **…** and toggle.
        if "**" in line:
            p = doc.add_paragraph()
            for i, seg in enumerate(line.split("**")):
                if not seg:
                    continue
                r = p.add_run(seg)
                r.bold = (i % 2 == 1)
            continue

        doc.add_paragraph(line)

    buf = io.BytesIO()
    doc.save(buf)
    return buf.getvalue()
