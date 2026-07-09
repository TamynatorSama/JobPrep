"""Chat-history context management.

The frontend resends a thread's ENTIRE transcript as `history` on every message,
and the chat route used to replay all of it into the prompt — so cost, latency,
and context-window pressure grew without bound on long coach threads and mock
interviews.

This module keeps the conversation bounded without losing the older context:

  * `split_history(...)` — keep only the most RECENT turns (within a char budget)
    verbatim. Those go straight into the prompt as messages.
  * `render_transcript(...)` — render the OLDER turns as a markdown transcript.
    The chat route feeds this into the existing RAG corpus (rag.build_context),
    so exact older details are retrievable on demand for the current question.
  * `summarize_older(...)` — a cheap fast-tier call that compresses the older
    turns into a short running recap added to the system prompt, preserving the
    arc of the conversation (e.g. so a mock interview's final per-question
    feedback still "remembers" the whole session).

Everything here is best-effort and stateless: nothing is persisted to disk
(the frontend already owns the durable thread), and any failure degrades to
"older turns just aren't summarized", never breaks the chat stream.
"""
from __future__ import annotations

import hashlib
from collections import OrderedDict
from typing import List, Sequence, Tuple

import llm_provider as llm_factory
from models import ChatMessage, LLMConfig

# How many chars of the most recent turns to keep VERBATIM before older turns
# get rolled into summary + RAG. ~4 chars/token, so ~6000 chars ≈ ~1.5k tokens
# of recent dialogue — comfortably covers the last several exchanges. Tunable.
RECENT_CHAR_BUDGET = 6000
# Always keep at least this many of the newest messages verbatim, even if a
# single turn blows the budget — the model needs the immediate exchange intact.
MIN_RECENT = 2
# Don't bother summarizing a trivially small older block; RAG alone covers it.
MIN_OLDER_CHARS = 800


def split_history(
    history: Sequence[ChatMessage],
    budget: int = RECENT_CHAR_BUDGET,
    min_recent: int = MIN_RECENT,
) -> Tuple[List[ChatMessage], List[ChatMessage]]:
    """Split `history` into (older, recent).

    `recent` is the newest run of turns that fits in `budget` chars (but always
    at least `min_recent` turns); `older` is everything before it. Order is
    preserved (oldest-first) in both lists.
    """
    recent: List[ChatMessage] = []
    total = 0
    for turn in reversed(list(history)):
        c = len(turn.content or "")
        if recent and total + c > budget and len(recent) >= min_recent:
            break
        recent.append(turn)
        total += c
    recent.reverse()
    older = list(history[: len(history) - len(recent)])
    return older, recent


def _speaker(role: str, mode: str) -> str:
    if role == "user":
        return "Candidate"
    return "Interviewer" if mode == "interviewer" else "Assistant"


def render_transcript(turns: Sequence[ChatMessage], mode: str = "coach") -> str:
    """Render turns as a markdown transcript suitable for RAG ingestion."""
    return "\n\n".join(
        f"**{_speaker(t.role, mode)}:** {t.content}".strip()
        for t in turns
        if (t.content or "").strip()
    )


_SUMMARY_PROMPT_COACH = """\
Summarize the earlier part of this interview-prep coaching conversation so it can
serve as memory for the rest of the chat. Capture: the user's goal/role, key
facts about them, advice already given, and any decisions or open threads. Be
factual and concise (under ~150 words). Output plain prose — no preamble, no
markdown headers.

=== EARLIER CONVERSATION ===
{transcript}
"""

_SUMMARY_PROMPT_INTERVIEWER = """\
You are compressing the earlier part of a LIVE mock interview into a recap the
interviewer will use to stay consistent and to write final feedback later.
For each question already asked, note: the question's topic, and how strong the
candidate's answer was (with one concrete detail). Also note any red flags or
standout strengths. Be factual and concise (under ~180 words). Output plain
prose — no preamble, no markdown headers. Do NOT invent turns that aren't below.

=== EARLIER INTERVIEW TURNS ===
{transcript}
"""


# Recap cache. The chat route calls summarize_older on EVERY message of a long
# thread, and the older block is identical between turns until enough new turns
# roll past the recent-budget — so without a cache each message pays an extra
# serial LLM call (latency before the stream even starts) for the same recap.
# Keyed by content hash; tiny LRU since threads in one app session are few.
_summary_cache: OrderedDict[str, str] = OrderedDict()
_SUMMARY_CACHE_MAX = 64


async def summarize_older(
    cfg: LLMConfig,
    older: Sequence[ChatMessage],
    mode: str = "coach",
) -> str:
    """Compress older turns into a short recap. Best-effort: returns "" when
    there's too little to summarize or the call fails."""
    transcript = render_transcript(older, mode)
    if len(transcript) < MIN_OLDER_CHARS:
        return ""
    key = hashlib.sha1(f"{mode}\x00{transcript}".encode("utf-8")).hexdigest()
    if key in _summary_cache:
        _summary_cache.move_to_end(key)
        return _summary_cache[key]
    template = (
        _SUMMARY_PROMPT_INTERVIEWER if mode == "interviewer" else _SUMMARY_PROMPT_COACH
    )
    prompt = template.format(transcript=transcript)
    try:
        text, _model = await llm_factory.generate_raw(
            cfg, prompt, tier="fast", temperature=0.2,
        )
    except Exception:  # noqa: BLE001 — recap is optional; never break chat
        return ""
    summary = (text or "").strip()
    if summary:
        _summary_cache[key] = summary
        while len(_summary_cache) > _SUMMARY_CACHE_MAX:
            _summary_cache.popitem(last=False)
    return summary
