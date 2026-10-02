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
# The older/recent boundary moves in steps of this many messages (3 exchanges)
# when the extra verbatim text that costs is small (short turns, like a mock
# interview's). If it moved every turn, the recap — and so the system prompt —
# would change on every message: a summary call each turn, and a prompt that
# never matches the provider's prompt cache. Long turns (coach replies) skip the
# step: keeping them verbatim would cost more than the recap call saves.
SUMMARY_STEP = 6
STEP_EXTRA_CHARS = RECENT_CHAR_BUDGET // 2


def split_history(
    history: Sequence[ChatMessage],
    budget: int = RECENT_CHAR_BUDGET,
    min_recent: int = MIN_RECENT,
) -> Tuple[List[ChatMessage], List[ChatMessage]]:
    """Split `history` into (older, recent).

    `recent` is the newest run of turns that fits in `budget` chars (but always
    at least `min_recent` turns); `older` is everything before it, rounded down
    to a whole number of SUMMARY_STEP messages when that keeps at most
    STEP_EXTRA_CHARS more verbatim. Order is preserved (oldest-first) in both
    lists.
    """
    kept = 0
    total = 0
    for turn in reversed(list(history)):
        c = len(turn.content or "")
        if kept and total + c > budget and kept >= min_recent:
            break
        kept += 1
        total += c
    n_older = len(history) - kept
    stepped = n_older - n_older % SUMMARY_STEP
    if sum(len(t.content or "") for t in history[stepped:n_older]) <= STEP_EXTRA_CHARS:
        n_older = stepped
    return list(history[:n_older]), list(history[n_older:])


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


_ROLL_PROMPT_COACH = """Below is the running summary of an interview-prep coaching conversation, then
the turns that came after it. Rewrite the summary so it also covers the new
turns: the user's goal/role, key facts about them, advice already given, and any
decisions or open threads. Be factual and concise (under ~150 words). Output
plain prose — no preamble, no markdown headers.

=== SUMMARY SO FAR ===
{summary}

=== LATER TURNS ===
{transcript}
"""

_ROLL_PROMPT_INTERVIEWER = """Below is the running recap of a LIVE mock interview, then the turns that came
after it. Rewrite the recap so it also covers the new turns. For each question
asked so far, note the question's topic and how strong the candidate's answer
was (with one concrete detail); also note any red flags or standout strengths.
Be factual and concise (under ~180 words). Output plain prose — no preamble, no
markdown headers. Do NOT invent turns that aren't in the recap or below.

=== RECAP SO FAR ===
{summary}

=== LATER TURNS ===
{transcript}
"""


# Recap cache, keyed by a hash of the older turns (chained turn by turn, so
# every prefix has its own key). The chat route calls summarize_older on EVERY
# message of a long thread: an unchanged older block is a cache hit, and a block
# that grew rolls the cached recap of its longest summarized prefix forward over
# just the new turns — so a recap call reads ~one exchange plus the old recap,
# not the whole thread again. Tiny LRU: threads in one app session are few.
_summary_cache: OrderedDict[str, str] = OrderedDict()
_SUMMARY_CACHE_MAX = 64


def _prefix_keys(turns: Sequence[ChatMessage], mode: str) -> List[str]:
    """keys[m] identifies turns[:m] (keys[0] = the empty prefix)."""
    h = hashlib.sha1(mode.encode("utf-8"))
    keys = [h.hexdigest()]
    for t in turns:
        h.update(f"\x00{t.role}\x00{t.content or ''}".encode("utf-8"))
        keys.append(h.copy().hexdigest())
    return keys


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
    keys = _prefix_keys(older, mode)
    if keys[-1] in _summary_cache:
        _summary_cache.move_to_end(keys[-1])
        return _summary_cache[keys[-1]]
    interviewer = mode == "interviewer"
    prompt = (_SUMMARY_PROMPT_INTERVIEWER if interviewer else _SUMMARY_PROMPT_COACH).format(
        transcript=transcript)
    for m in range(len(older) - 1, 0, -1):
        prev = _summary_cache.get(keys[m])
        if prev:
            prompt = (_ROLL_PROMPT_INTERVIEWER if interviewer else _ROLL_PROMPT_COACH).format(
                summary=prev, transcript=render_transcript(older[m:], mode))
            break
    try:
        text, _model = await llm_factory.generate_raw(
            cfg, prompt, tier="fast", temperature=0.2,
        )
    except Exception:  # noqa: BLE001 — recap is optional; never break chat
        return ""
    summary = (text or "").strip()
    if summary:
        _summary_cache[keys[-1]] = summary
        while len(_summary_cache) > _SUMMARY_CACHE_MAX:
            _summary_cache.popitem(last=False)
    return summary
