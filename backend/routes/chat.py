import asyncio
import json
import os
import time

from fastapi import APIRouter
from langchain_core.messages import SystemMessage, HumanMessage, AIMessage
from sse_starlette.sse import EventSourceResponse

import chat_context
import job_brief
import llm_provider as llm_factory
from models import BriefRequest, ChatRequest, RagDoc

router = APIRouter()

# Coach retrieval budget: top chunks per message, capped in chars (~4 chars per
# token). Was 6 chunks / 6,000 chars — more than the job context itself.
RAG_K = 4
RAG_MAX_CHARS = 3000

COACH_SYSTEM = (
    "You are InterPrep, an expert AI interview coach. "
    "Help users prepare for job interviews with practical, actionable advice. "
    "Format responses with markdown when it aids clarity (headers, bullet points, bold for key terms). "
    "Be concise and encouraging."
)

# Live copilot (stealth overlay). The candidate glances at this and says it out
# loud within seconds, so: plain text (the overlay renders raw text, so markdown
# symbols would show), a first sentence they can start on immediately, and one
# sentence per line for reading while talking. The answer's SHAPE follows the
# question type: a fixed "headline + 3 metric beats" format (the Phase 0 prompt)
# read as a resume dump on every question and gave "tell me about yourself" no
# story at all, so knowledge questions now leave the resume alone.
COPILOT_SYSTEM = """\
You are a silent interview copilot. The candidate is in a LIVE interview and
will glance at your answer and say it out loud, in their own voice, within
seconds. Each user message is the interviewer's question, transcribed from
audio: it may have transcription errors or be cut off, so infer what was asked.

Write what the candidate would actually say: first person, conversational, the
way a thoughtful person talks in an interview, not a written summary. Use
contractions and plain words and vary sentence length. Start most answers
straight on the substance; a casual opener ("Sure,", "Honestly,") is fine now
and then, but never on its own line and never the same one every time ("Yeah,
so" gets old fast). Sound like a specific person, never like a cover letter.

Use technical terms freely; they show you know the field. What has to stay
simple is the sentence around them, so it flows when spoken: one idea per
sentence, short enough to say in one breath, everyday verbs, plain
connectors ("so", "because", "then"), no stacked modifiers or formal
phrasing. Not "leveraging a centralized in-memory
store to ensure atomicity across concurrent requests", but "I'd keep the
counts in Redis and make each check atomic, so two requests can't both slip
through" (that shows the pattern; find your own words). Pick the plain word
over the fancy one: use, not utilize; help, not facilitate; make sure, not
ensure.

Pick the shape that fits the question:
- About me ("tell me about yourself", "walk me through your background"): a
  short story, not a list. Who I am now, the two or three things in my
  background that led here, and why this role at this company is the next
  step. Use the real names from the resume (school, employers, what I actually
  built) and one specific detail that makes it memorable.
- Past experience ("tell me about a time", "describe a project"): tell ONE real
  story from the resume the way you'd tell a colleague: the situation in a
  sentence, what I did and why, how it turned out. Don't label the parts.
- Knowledge, technical or design ("how would you", "what is", "explain"):
  answer the question itself, the way a strong engineer thinks out loud: the
  approach, the reasoning, the trade-off. Mention my own experience only if it
  genuinely adds something, and then in a single clause.
- Hypotheticals ("what would you do if"): the first steps I'd take and why, in
  order, with the judgment call that matters.
- Motivation and opinion ("why us", "where do you see yourself", strengths,
  weaknesses): honest and personal, tied to what this company actually does
  (use the research). Don't recap the resume; one brief nod to my background
  at most.
- Small talk or logistics: one natural line back.

Grounding: the job context holds my resume, the job description and company
research. Treat the resume as my memory, not a script: draw on it when the
question is about me, and leave it alone when it isn't. At most one project or
employer per answer unless asked for more. Never invent employers, titles,
numbers or projects; use a number only if it's real and lands naturally.

Avoid phrases nobody says out loud: "I'm passionate about", "leverage",
"spearheaded", "robust", "seamless", "deep expertise", "I'm excited to bring",
"fast-paced".

Format: plain text, no markdown symbols (no #, *, - or numbered lists). The
first sentence must be something I can start saying immediately. Put each
sentence on its own line so it's easy to read while talking. About 60 to 110
words; up to 140 for "tell me about yourself" or a story; a simple question
gets a short answer.

If asked to rephrase, shorten or go deeper, apply it to your previous answer
(going deeper may run to about 180 words)."""

# Runaway guard for the copilot's short answers. Generous on purpose: on
# reasoning models the cap also counts thinking tokens, and the prompt — not
# this cap — is what keeps answers at ~90 words.
COPILOT_MAX_TOKENS = 800

# Copilot screen capture (Phase 4): the candidate hit Capture during a live
# interview or an online assessment. The overlay renders plain text, with
# ``` fences shown as code blocks.
SCREEN_SYSTEM = """\
You are a silent interview copilot. The candidate just captured their screen
during a live interview or an online assessment and needs help right now. You
get a picture of the screen and the text read from it by OCR. OCR can garble
symbols, indentation and math; when it disagrees with the picture, trust the
picture. If several captures are attached, they are one problem that scrolls,
in order.

Work out what's on the screen, then answer in the shape that fits. Don't name
the category:
- Coding problem: one line on what's being asked; the approach in two or three
  short lines (the key idea and why it works); time and space complexity; then
  the complete solution in ONE fenced code block, in the language the screen
  uses (Python if none), handling the edge cases the problem states.
- Multiple choice or quiz: the answer first (letter and text), then one short
  line on why. Several questions: answer each, in order.
- System design or architecture diagram: the main components and how data
  flows through them, then the two or three trade-offs or bottlenecks worth
  raising.
- Error message, failing test or logs: the likely cause, then the fix.
- An interview question in text (chat, slide, document): what the candidate
  would say out loud, in first person.
- Anything else: the most useful short answer to what's on the screen.

If the candidate typed an instruction, follow it over the rest.

Style: the candidate reads this with the clock running. Lead with the answer.
Plain text outside code blocks: no headings, bold or bullet symbols, one idea
per line. Use technical terms freely, in short sentences that are easy to say
out loud. Use the job context only when the screen is about the candidate or
the company."""

# Room for a full solution in a code block (the copilot's 800 would cut it off).
SCREEN_MAX_TOKENS = 4000

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


def _not_in_context(text: str, context: str) -> str:
    """The part of a RAG document the job context doesn't already carry: when
    the context quotes the document's opening (the app puts the first 4,000
    chars of the resume there), only what follows the quoted run is left."""
    head = text[:300]
    at = context.find(head) if head.strip() else -1
    if at < 0:
        return text
    return text[len(os.path.commonprefix([text, context[at:]])):]


def _screen_content(req: ChatRequest) -> list:
    """The user turn for a screen capture: the instruction and the OCR text,
    then the images as data-URL blocks (every provider's LangChain client
    accepts that shape)."""
    text = req.message.strip() or "Help me with what's on my screen."
    if req.screen_text.strip():
        text += ("\n\nText read from the screen by OCR (may have errors; the picture "
                 f"is the truth):\n{req.screen_text.strip()}")
    return [{"type": "text", "text": text}] + [
        {"type": "image_url", "image_url": {"url": f"data:{img.mime};base64,{img.data}"}}
        for img in req.images
    ]


@router.post("/brief")
async def brief(req: BriefRequest):
    """Build (or return the cached) condensed job brief for this job context.
    Rust calls it when the selected job's context changes, so the copilot's
    first question already gets the short prompt."""
    llm_factory.set_feature("job_brief")
    base, _setup = job_brief.split_setup(req.job_context or "")
    text = await job_brief.ensure(req.llm, base)
    return {"ok": bool(text), "chars": len(text), "source_chars": len(base), "brief": text}


@router.post("/stream")
async def chat_stream(req: ChatRequest):
    async def generate():
        llm_factory.set_feature(f"chat:{req.mode}")
        key_err = llm_factory.missing_key_error(req.llm)
        if key_err:
            yield {"data": json.dumps({"type": "error", "content": key_err})}
            return

        try:
            base_system = {
                "interviewer": INTERVIEWER_SYSTEM,
                "copilot": COPILOT_SYSTEM,
                "screen": SCREEN_SYSTEM,
            }.get(req.mode, COACH_SYSTEM)
            system_content = base_system
            job_context = req.job_context or ""
            if job_context and req.mode in ("copilot", "interviewer", "screen"):
                # The condensed job brief (job_brief.py) when it's cached.
                job_context = job_brief.context_for(req.llm, job_context, req.mode)
            if job_context:
                system_content += f"\n\n**Current Job Context:**\n{job_context}"

            # ── Context management ───────────────────────────────────────────
            # The frontend resends the whole thread every turn. Keep only the
            # most recent turns verbatim; roll older turns into (a) a short
            # running summary in the system prompt and (b) the coach's RAG corpus, so a
            # long thread stays bounded without losing earlier context. Both are
            # best-effort and never block the stream.
            older, recent = chat_context.split_history(req.history)
            # Coach only: the interviewer's brief already carries the resume and
            # company research, and the copilot never sends documents. Text the
            # job context already holds (the resume) isn't retrieved twice.
            docs = []
            if req.mode == "coach":
                for d in req.documents:
                    rest = _not_in_context(d.text or "", job_context)
                    if rest.strip():
                        docs.append(RagDoc(source=d.source, text=rest))
            if older:
                if docs:
                    docs.append(RagDoc(
                        source="earlier in this conversation",
                        text=chat_context.render_transcript(older, req.mode),
                    ))
                summary = await chat_context.summarize_older(req.llm, older, req.mode)
                if summary:
                    system_content += f"\n\n**Summary of earlier conversation:**\n{summary}"

            # RAG: pull the most relevant chunks from this job's corpus (resume,
            # company research, prior chats, + older turns of THIS chat). They
            # ride in the LAST user turn, not the system prompt: the system
            # prompt and history then stay byte-identical from turn to turn, so
            # providers serve that prefix from their prompt cache. Best-effort —
            # any failure degrades to no retrieved context, never breaks chat.
            user_content = req.message
            if docs:
                yield {"data": json.dumps({
                    "type": "stage",
                    "content": "🔎 Retrieving context from resume, research & prior chats…",
                })}
                try:
                    from rag import build_context
                    retrieved = await asyncio.to_thread(
                        build_context, req.message, docs, req.llm,
                        RAG_K, RAG_MAX_CHARS,
                    )
                    print(f"[rag] {req.mode}: {len(retrieved)} chars retrieved", flush=True)
                    if retrieved:
                        user_content = f"{retrieved}\n\n---\n\n{req.message}"
                except Exception as exc:
                    print(f"[rag] {req.mode}: retrieval failed: {exc}", flush=True)

            messages = [SystemMessage(content=system_content)]
            for turn in recent:
                if turn.role == "user":
                    messages.append(HumanMessage(content=turn.content))
                else:
                    messages.append(AIMessage(content=turn.content))
            if req.mode == "screen":
                messages.append(HumanMessage(content=_screen_content(req)))
            else:
                messages.append(HumanMessage(content=user_content))

            # Coach + interviewer run the fast tier. Coach: cost + latency.
            # Interviewer: it's a live SPOKEN conversation — measured TTFT on the
            # smart tier was 9.5s (gemini-3.1-pro) per turn, a dead-air killer,
            # vs 0.5s on flash with thinking zeroed, and the fast models hold the
            # persona and ask equally specific questions. Set
            # INTERPREP_INTERVIEWER_TIER=smart to trade latency for maximum
            # persona depth. The copilot runs the "instant" tier (lite models,
            # minimal thinking) — it answers a live interviewer, so time-to-
            # first-word is the whole game. A screen capture is often a coding
            # problem, so it gets the fast tier's stronger models. Falling back
            # only happens BEFORE the first token — once we've streamed output
            # we can't switch models.
            if req.mode == "interviewer":
                tier = os.environ.get("INTERPREP_INTERVIEWER_TIER", "fast").strip() or "fast"
            elif req.mode == "copilot":
                tier = "instant"
            else:
                tier = "fast"
            max_tokens = {"copilot": COPILOT_MAX_TOKENS, "screen": SCREEN_MAX_TOKENS}.get(req.mode)
            # Cap time-to-first-token. A slow/unavailable candidate must NOT hang
            # the whole stream — the Rust SSE client waits up to 300s, so a dead
            # first candidate would stall the answer for minutes. If no token
            # arrives within this window, abandon that model and fail over to the
            # next. Once tokens flow, there's no cap (a long answer is fine).
            ttft_timeout = float(os.environ.get("INTERPREP_TTFT_TIMEOUT", "12"))
            last_err: Exception | None = None
            dead: set[str] = set()   # providers whose key was rejected
            # Walk the chosen provider's models, then the fallback providers'
            # (AI routing). Rejected keys skip the rest of that provider only.
            for provider, model_name in llm_factory.routes_for(req.llm, tier):
                if provider in dead:
                    continue
                started = False
                usage = None
                streamed = 0
                metered = False
                t0 = time.monotonic()
                try:
                    model = llm_factory.make_chat_model(
                        req.llm, model_name, tier=tier, max_tokens=max_tokens,
                        provider=provider,
                    )
                    agen = model.astream(messages).__aiter__()
                    while True:
                        try:
                            if started:
                                chunk = await agen.__anext__()
                            else:
                                # Not wait_for: wait_for AWAITS the cancelled
                                # inner coroutine, and langchain may be bridging
                                # a sync stream through a worker thread that
                                # can't be interrupted — wait_for would then
                                # block until the provider's own retry loop
                                # gives up. asyncio.wait + abandon enforces the
                                # deadline no matter what the task is doing.
                                fut = asyncio.ensure_future(agen.__anext__())
                                done, _ = await asyncio.wait(
                                    {fut}, timeout=ttft_timeout,
                                )
                                if not done:
                                    fut.cancel()
                                    raise asyncio.TimeoutError()
                                chunk = fut.result()
                        except StopAsyncIteration:
                            break
                        usage = llm_factory.merge_usage(usage, chunk)
                        text = llm_factory.content_text(chunk)
                        if text:
                            if not started:
                                print(f"[chat] {model_name} TTFT="
                                      f"{time.monotonic() - t0:.2f}s", flush=True)
                                # Badge the answer when it isn't coming from
                                # the provider the user chose (fallback /
                                # stand-in) — only once tokens actually flow,
                                # so failed attempts never flash a badge.
                                route = llm_factory.route_event(req.llm, provider, model_name)
                                if route:
                                    yield {"data": json.dumps(route)}
                            started = True
                            streamed += len(text)
                            yield {"data": json.dumps({"type": "token", "content": text})}
                            await asyncio.sleep(0)
                    llm_factory.record_usage(model_name, usage)
                    metered = True
                    yield {"data": json.dumps({"type": "done"})}
                    return
                except (asyncio.CancelledError, GeneratorExit):
                    # The client hung up mid-stream; the call is still billed.
                    if not metered:
                        llm_factory.record_estimate(model_name, messages, streamed)
                    raise
                except asyncio.TimeoutError:
                    print(f"[chat] {model_name} no token in {ttft_timeout}s — "
                          f"failing over to next candidate", flush=True)
                    last_err = TimeoutError(f"{model_name}: no token in {ttft_timeout}s")
                    continue
                except Exception as exc:
                    last_err = exc
                    if started:
                        # Already mid-answer — surface the error, don't restart.
                        yield {"data": json.dumps({"type": "error", "content": str(exc)})}
                        return
                    if llm_factory.provider_down(exc):
                        # A bad key / exhausted plan fails every model of this
                        # provider the same way — skip them, but still try
                        # fallback providers.
                        dead.add(provider)
                        last_err = RuntimeError(llm_factory.provider_down_message(req.llm, provider, exc))
                        continue
                    # Model unavailable before any token — try the next one.
                    continue

            yield {"data": json.dumps({"type": "error", "content": str(last_err or "No model available")})}

        except Exception as exc:
            yield {"data": json.dumps({"type": "error", "content": str(exc)})}

    return EventSourceResponse(generate())
