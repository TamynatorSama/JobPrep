"""Resume-task model orchestrator.

Picks the cheapest model tier that still clears the quality bar for the
resume-tailoring flow, instead of pinning every call to the most expensive
("smart") tier. Two decisions live here:

  1. `route_resume(...)`  — an LLM *router*: one cheap, fast-tier call classifies
     how hard this job/resume pairing is and chooses the DRAFT tier ("mid" for
     easy/medium jobs, "smart" for hard ones). The router runs on the cheapest
     model with a tiny prompt and falls back to "smart" on any failure, so it
     can only ever save money — never block or degrade the flow.

  2. `refine_decision(...)` — a heuristic on the draft's own scorecard: skip the
     second (refine) pass entirely when the draft is already strong, escalate it
     to "smart" when the draft is weak, otherwise run it on "mid".

Cost effect vs the old always-two-smart-calls behavior:
    easy   -> mid draft + skipped refine = 1 mid call
    medium -> mid draft + mid refine     = 2 mid calls
    hard   -> smart draft + smart refine = 2 smart calls (+ 1 cheap router call)
"""
from __future__ import annotations

from dataclasses import dataclass

import llm_provider as llm_factory
from models import LLMConfig

# ── Tunable thresholds ───────────────────────────────────────────────────────
# A draft is "strong" (skip refine) only if it clears ALL of these.
STRONG_VERBATIM = 85
STRONG_QUANT = "Pass"
STRONG_TITLE = "Yes"
# A draft is "weak" (refine on smart) if ANY of these trip.
WEAK_VERBATIM = 70

# Keep the router prompt cheap: a difficulty signal needs only a slice of each.
_JD_CHARS = 3000
_RESUME_CHARS = 2000


@dataclass
class RoutingPlan:
    draft_tier: str          # "mid" | "smart"
    complexity: str          # "low" | "medium" | "high"
    reason: str              # short human-readable justification (for the log/UI)
    routed: bool = True      # False when we returned a default without an LLM call


def _default_plan(reason: str, *, routed: bool = False) -> RoutingPlan:
    """Safe fallback: behave like the old code (smart draft)."""
    return RoutingPlan(draft_tier="smart", complexity="medium", reason=reason, routed=routed)


ROUTER_PROMPT = """You are a model-selection ROUTER for a resume-tailoring pipeline.
You do NOT write the resume. You only judge how hard it is to produce a
high-quality, ATS-tailored resume + cover letter for the JOB below from the
candidate's RESUME, then pick which model tier should draft it.

Pick "mid" (a cheaper, capable model) when the job is routine and the resume
clearly covers it: common role, clear JD, strong/obvious match, junior-to-mid
seniority. Pick "smart" (the strongest model) when it is genuinely hard: senior/
staff/lead/exec roles, sparse or weakly-matching resume, niche or highly
technical domain, or a JD with many specialized must-haves.

Return ONLY a JSON object (no prose, no code fences):
{ "draft_tier": "mid" | "smart",
  "complexity": "low" | "medium" | "high",
  "reason": "<=15 words" }
"""


async def route_resume(
    cfg: LLMConfig,
    *,
    company: str,
    role: str,
    jd: str,
    resume_text: str,
) -> RoutingPlan:
    """Classify draft difficulty with one cheap fast-tier call.

    Never raises — any failure (no JD, pinned model, model error, bad JSON)
    returns a safe default that reproduces the old smart-draft behavior.
    """
    # A user-pinned model (Settings override) forces every tier to that one
    # model, so routing would be meaningless — skip the call.
    if cfg.model.strip():
        return _default_plan("model pinned in Settings — routing skipped")
    if not (jd or "").strip():
        return _default_plan("no JD provided — defaulting to smart draft")

    prompt = (
        ROUTER_PROMPT
        + "\n\n=== JOB ===\n"
        + f"Company: {company}\nRole: {role}\n\n"
        + f"=== JOB DESCRIPTION (truncated) ===\n{jd[:_JD_CHARS]}\n\n"
        + f"=== CANDIDATE RESUME (truncated) ===\n{(resume_text or '')[:_RESUME_CHARS]}\n"
    )
    try:
        data, _model = await llm_factory.generate_json(
            cfg, prompt, tier="fast", temperature=0,
        )
    except Exception as exc:  # noqa: BLE001 — router must never break the flow
        return _default_plan(f"router unavailable ({type(exc).__name__}) — defaulting to smart")

    tier = str(data.get("draft_tier", "")).strip().lower()
    if tier not in ("mid", "smart"):
        return _default_plan("router returned no tier — defaulting to smart")
    complexity = str(data.get("complexity", "medium")).strip().lower()
    if complexity not in ("low", "medium", "high"):
        complexity = "medium"
    reason = str(data.get("reason", "")).strip()[:120] or "router decision"
    return RoutingPlan(draft_tier=tier, complexity=complexity, reason=reason, routed=True)


def refine_decision(plan: RoutingPlan, scorecard: dict | None) -> tuple[bool, str]:
    """Decide whether to run the refine pass, and on which tier.

    Returns (should_refine, tier). `tier` is "" when skipping.
    """
    sc = scorecard or {}

    def _as_int(v) -> int | None:
        try:
            return int(float(v))
        except (TypeError, ValueError):
            return None

    verbatim = _as_int(sc.get("verbatim_match_score"))
    quant = str(sc.get("quantification_check", "")).strip()
    title = str(sc.get("role_title_alignment", "")).strip()
    hire = str(sc.get("hire_recommendation", "")).strip()

    # Strong draft → skip the refine pass entirely.
    if (
        verbatim is not None and verbatim >= STRONG_VERBATIM
        and quant == STRONG_QUANT
        and title == STRONG_TITLE
    ):
        return False, ""

    # Weak draft (or a hard job) → spend the smart tier closing the gaps.
    if (
        (verbatim is not None and verbatim < WEAK_VERBATIM)
        or hire == "No Hire"
        or plan.complexity == "high"
    ):
        return True, "smart"

    # Middle ground → a cheaper refine is enough.
    return True, "mid"
