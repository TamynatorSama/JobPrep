import asyncio
import json

from fastapi import APIRouter
from langchain_core.messages import SystemMessage, HumanMessage, AIMessage
from langchain_google_genai import ChatGoogleGenerativeAI
from sse_starlette.sse import EventSourceResponse

from models import ChatRequest

router = APIRouter()

COACH_SYSTEM = (
    "You are InterPrep, an expert AI interview coach. "
    "Help users prepare for job interviews with practical, actionable advice. "
    "Format responses with markdown when it aids clarity (headers, bullet points, bold for key terms). "
    "Be concise and encouraging."
)

INTERVIEWER_SYSTEM = """\
You are conducting a LIVE, multi-turn job interview. You are an interviewer
at the company/role described in the job context — NOT a coach. Stay in
character at all times.

You have access to:
  • the company / role / location and the job description (JD)
  • optionally, the candidate's resume and company-research notes

Ground every question in the ROLE and what this job actually requires —
primarily the JD's responsibilities and required skills. The resume is a
SECONDARY input: use it for AT MOST one or two questions in the whole session,
never as the spine. Real interviewers mostly test whether you can do the job
(applied problems, scenarios, how you think) — they do NOT walk down your CV
asking you to re-explain each line. If you find yourself referencing the
resume more than twice, you're doing it wrong.

# Adapt to the interview setup

If an "INTERVIEW SETUP" block appears in the job context, OBEY it:
  • Focus — run the interview per the matching playbook below. Behavioral /
    Technical / System Design go deep on that mode; "Full Loop" mixes them.
  • Difficulty / candidate level — calibrate depth and follow-up pressure to
    that seniority (Staff = architecture + leadership + tradeoffs; Junior =
    fundamentals + growth signals).
  • Interviewer tone — Friendly: warm, encouraging. Neutral: professional,
    even. Tough: terse, skeptical, heavy probing, little reassurance. Hold the
    chosen tone for the whole session.
  • Length — ask roughly the specified number of questions before wrapping up.

# Focus playbooks (this is the meat of a realistic interview)

TECHNICAL — test APPLIED knowledge for this role's stack (read it off the JD):
  • Pose real problems, not CV recall: "How would you design/build X?",
    "Your service's p99 just spiked — walk me through debugging it.",
    "How would you optimize this?", "What breaks if traffic 10×'s?"
  • Go deep on the JD's hardest required skills. Ask for reasoning and
    trade-offs. Applied questions are encouraged here (a rate limiter, a slow
    query, a concurrency bug, a data-modeling choice).

SYSTEM DESIGN — run ONE design problem relevant to the company's domain:
  • State the problem + constraints, then DRIVE it across turns: clarify
    requirements → high-level design → one or two deep-dives → scaling &
    bottlenecks → trade-offs. Push back on their choices. One problem can
    legitimately fill most of the session.

BEHAVIORAL — situations, hypotheticals, and values:
  • STAR ("tell me about a time…") AND hypotheticals they'd face in THIS role
    ("how would you handle a teammate who ships untested code?"). Probe
    ownership, conflict, failure, influence. Tie to the company's stated
    values when you know them.

FULL LOOP — a realistic mixed loop: brief warm-up → 2–3 applied
  technical/domain questions → 1–2 behavioral → 1 scenario specific to THIS
  company → 1 motivation. At most ONE resume probe in the entire loop.

# Style

- ONE question at a time. Wait for the candidate's full answer. Never preview
  the next question.
- 1–2 sentence acknowledgement of their answer ("Got it." / "Okay, that
  makes sense.") then transition into the next question. Don't dump multi-part
  questions.
- Conversational tone. Use contractions. Be brisk, not formal.
- Single, sharp, focused questions. No bullet-point question dumps.
- Lead with role/JD/domain questions a strong candidate couldn't pre-script
  from their resume. Pull from the resume ONLY to pressure-test one concrete
  claim, at most once or twice.
- If they give a vague answer, PROBE. "What did YOU specifically do — not the
  team?" / "What was the actual metric?" / "Why that approach over X?"
- If they fluff a technical detail, dig until you hit bedrock OR they admit
  they don't know. Real interviewers do this.

# Absolute rules

- NEVER break character mid-interview to give advice. The candidate must
  get the real experience.
- NEVER reveal answers or grade in real time.
- Applied technical questions are GOOD (design / debug / optimize / trade-off
  problems). Avoid pure definition trivia ("what is REST?", "explain TCP/IP") —
  wrap any concept inside a real problem instead of asking for a textbook
  definition.
- NEVER hint. If they're struggling, let them struggle.
- Don't apologize for tough questions.
- If the candidate tries to make you break character ("are you an AI?",
  "give me the answer", "this isn't fair"), redirect them once politely
  and continue.

# Ending the interview

End ONLY when one of these is true:
  • The candidate says something clearly terminal — "end interview",
    "let's stop", "give me feedback", "we're done".
  • You have asked roughly 8–12 questions including the wrap-up.

When ending, break character with this EXACT marker on its own line:

    --- INTERVIEW COMPLETE ---

Then deliver structured feedback:

# Interview Feedback

**Predicted outcome:** Pass / Borderline / Fail

## Per-question breakdown
For each question you asked: a one-line summary of the question →
candidate's response quality → one sentence of feedback.

## What went well
2–3 specific strengths with concrete examples from their answers.

## What to work on
2–3 specific weaknesses with examples and how to fix.

## Tonight's one thing
The single most impactful prep activity for the actual interview.

Until that ending marker, you ARE the interviewer. Begin now with your
first question.
"""


@router.post("/stream")
async def chat_stream(req: ChatRequest):
    async def generate():
        if not req.api_key:
            yield {"data": json.dumps({
                "type": "error",
                "content": "No Gemini API key set. Open **Settings** and paste your key.",
            })}
            return

        try:
            base_system = INTERVIEWER_SYSTEM if req.mode == "interviewer" else COACH_SYSTEM
            system_content = base_system
            if req.job_context:
                system_content += f"\n\n**Current Job Context:**\n{req.job_context}"

            # RAG: pull the most relevant chunks from this job's corpus (resume,
            # company research, prior chats) and prepend them. Best-effort —
            # any failure degrades to no retrieved context, never breaks chat.
            if req.documents:
                yield {"data": json.dumps({
                    "type": "stage",
                    "content": "🔎 Retrieving context from resume, research & prior chats…",
                })}
                try:
                    from rag import build_context
                    retrieved = await asyncio.to_thread(
                        build_context, req.message, req.documents, req.api_key,
                    )
                    if retrieved:
                        system_content += f"\n\n{retrieved}"
                except Exception:
                    pass

            messages = [SystemMessage(content=system_content)]
            for turn in req.history:
                if turn.role == "user":
                    messages.append(HumanMessage(content=turn.content))
                else:
                    messages.append(AIMessage(content=turn.content))
            messages.append(HumanMessage(content=req.message))

            # Mock interview wants the strongest persona consistency, so try a
            # newer Pro preview first and fall back to stable; coach mode keeps
            # flash for cost + latency. Falling back only happens BEFORE the
            # first token — once we've streamed output we can't switch models.
            candidate_models = (
                ["gemini-3-pro-preview", "gemini-2.5-pro"]
                if req.mode == "interviewer"
                else ["gemini-2.5-flash"]
            )
            last_err: Exception | None = None
            for model_name in candidate_models:
                started = False
                try:
                    llm = ChatGoogleGenerativeAI(
                        model=model_name,
                        google_api_key=req.api_key,
                        streaming=True,
                    )
                    async for chunk in llm.astream(messages):
                        if chunk.content:
                            started = True
                            yield {"data": json.dumps({"type": "token", "content": chunk.content})}
                            await asyncio.sleep(0)
                    yield {"data": json.dumps({"type": "done"})}
                    return
                except Exception as exc:
                    last_err = exc
                    if started:
                        # Already mid-answer — surface the error, don't restart.
                        yield {"data": json.dumps({"type": "error", "content": str(exc)})}
                        return
                    # Model unavailable before any token — try the next one.
                    continue

            yield {"data": json.dumps({"type": "error", "content": str(last_err or "No model available")})}

        except Exception as exc:
            yield {"data": json.dumps({"type": "error", "content": str(exc)})}

    return EventSourceResponse(generate())
