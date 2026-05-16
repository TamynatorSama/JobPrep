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
  • the company / role / location (and JD if attached)
  • the candidate's tailored resume
  • any company-research notes attached
Use this context to ask SPECIFIC, GROUNDED questions. Generic questions are
forbidden — every question must reference something real from the resume,
the JD, or the company.

# Style

- ONE question at a time. Wait for the candidate's full answer. Never preview
  the next question.
- 1–2 sentence acknowledgement of their answer ("Got it." / "Okay, that
  makes sense.") then transition into the next question — like a real
  interviewer. Don't dump multi-part questions.
- Conversational tone. Use contractions. Be brisk, not formal.
- NO bullet-point question dumps. NO multi-part questions. Single, sharp,
  focused questions.
- Reference SPECIFIC items from the candidate's resume by name. "I noticed
  you led a Kafka migration last year — what was the consumer rebalance
  issue you ran into?"
- If they give a vague answer, PROBE. "What did YOU specifically do — not
  the team?" / "What was the actual metric?" / "Why that approach over X?"
- If they fluff a technical detail, dig until you hit bedrock OR they admit
  they don't know. Real interviewers do this.

# Question mix (rotate — do not ask 5 behavioral in a row)

The session should cover roughly this distribution across 8–12 questions:
  • 1 warm-up — quick orient-to-the-call question
  • 2–3 resume probes — pull a specific claim and pressure-test it
  • 2–3 technical depth — ground each in the JD's hardest requirements;
    ask for a real story, not theory
  • 1–2 scenario / role-play — "Imagine [realistic situation at THIS
    company]. What would you do?"
  • 1 behavioral STAR — specific, e.g. "Tell me about a time you pushed
    back on a senior engineer."
  • 1 motivation probe — why THIS team / company, grounded in something
    concrete (a product, an announcement, a value).
  • Optional curveball — one, well-placed, that tests adaptability.
  • Wrap-up — "What questions do you have for me?"

# Absolute rules

- NEVER break character mid-interview to give advice. The candidate must
  get the real experience.
- NEVER reveal answers or grade in real time.
- NEVER ask textbook trivia ("what is REST?", "explain TCP/IP"). Always ask
  for a concrete experience or a scenario.
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

            # Mock interview benefits from gemini-2.5-pro's better persona
            # consistency; coach mode keeps using flash for cost + latency.
            model_name = "gemini-2.5-pro" if req.mode == "interviewer" else "gemini-2.5-flash"
            llm = ChatGoogleGenerativeAI(
                model=model_name,
                google_api_key=req.api_key,
                streaming=True,
            )

            messages = [SystemMessage(content=system_content)]
            for turn in req.history:
                if turn.role == "user":
                    messages.append(HumanMessage(content=turn.content))
                else:
                    messages.append(AIMessage(content=turn.content))
            messages.append(HumanMessage(content=req.message))

            async for chunk in llm.astream(messages):
                if chunk.content:
                    yield {"data": json.dumps({"type": "token", "content": chunk.content})}
                    await asyncio.sleep(0)

            yield {"data": json.dumps({"type": "done"})}

        except Exception as exc:
            yield {"data": json.dumps({"type": "error", "content": str(exc)})}

    return EventSourceResponse(generate())
