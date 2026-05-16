"""
Company research orchestrator.

Architecture:
  supervisor → (glassdoor | indeed | google | comparably | levels | repvue)* → compose → END

The supervisor LLM decides which research agent to dispatch next.
Each site agent uses Playwright + LLM navigation (LLMBrowserAgent).
The compose node generates the final structured report via streaming LLM.
"""
import json
from langgraph.graph import StateGraph, END
from langchain_core.messages import HumanMessage, SystemMessage
from langchain_google_genai import ChatGoogleGenerativeAI

from .state import CompanyResearchState
from .glassdoor_agent import GlassdoorAgent
from .indeed_agent import IndeedAgent
from .google_agent import GoogleAgent
from .comparably_agent import ComparablyAgent
from .levels_agent import LevelsAgent
from .repvue_agent import RepVueAgent

_ALL_AGENTS = ["glassdoor", "indeed", "google", "comparably", "levels", "repvue"]

_SUPERVISOR_SYSTEM = """\
You are a research coordinator preparing an interview candidate.
Given which sources have already been searched, decide which to search next.
Available sources: glassdoor, indeed, google, comparably, levels, repvue.
- repvue is only useful for sales, account executive, or revenue-focused roles; skip it for engineering/design/product/ops roles.
- levels is most useful for engineering, product, and design roles at tech companies.
- Always collect from all relevant sources before composing.
Respond with a single word: the next source name, or "compose" if all relevant sources are done."""


def _make_llm(api_key: str, streaming: bool = False) -> ChatGoogleGenerativeAI:
    return ChatGoogleGenerativeAI(
        model="gemini-2.5-flash",
        google_api_key=api_key,
        streaming=streaming,
    )


# ── Node: supervisor ──────────────────────────────────────────────────────────

def supervisor_node(state: CompanyResearchState) -> dict:
    completed = state.get("completed", [])
    remaining = [a for a in _ALL_AGENTS if a not in completed]

    if not remaining:
        return {"next_agent": "compose"}

    llm = _make_llm(state["api_key"])
    prompt = (
        f"Company: {state['company']}\n"
        f"Role: {state['role']}\n"
        f"Already collected: {completed}\n"
        f"Still available: {remaining}\n\n"
        f"Which source should be researched next? "
        f"Remember: repvue only for sales/revenue roles; levels most useful for tech roles. "
        f"Choose from: {remaining}, or 'compose' if all relevant sources are done."
    )
    resp = llm.invoke([
        SystemMessage(content=_SUPERVISOR_SYSTEM),
        HumanMessage(content=prompt),
    ])
    chosen = resp.content.strip().lower()
    if chosen == "compose":
        return {"next_agent": "compose"}
    if chosen not in remaining:
        chosen = remaining[0]
    return {"next_agent": chosen}


# ── Node factories ────────────────────────────────────────────────────────────

def make_glassdoor_node(llm):
    async def glassdoor_node(state: CompanyResearchState) -> dict:
        agent = GlassdoorAgent(llm, state["credentials"])
        data = await agent.research(
            state["company"], state["role"], state.get("location", "")
        )
        return {
            "glassdoor_data": data,
            "completed": state.get("completed", []) + ["glassdoor"],
        }
    return glassdoor_node


def make_indeed_node(llm):
    async def indeed_node(state: CompanyResearchState) -> dict:
        agent = IndeedAgent(llm, state["credentials"])
        data = await agent.research(state["company"], state["role"])
        return {
            "indeed_data": data,
            "completed": state.get("completed", []) + ["indeed"],
        }
    return indeed_node


def make_google_node(llm):
    async def google_node(state: CompanyResearchState) -> dict:
        agent = GoogleAgent(llm)
        data = await agent.research(
            state["company"], state["role"], state.get("location", "")
        )
        return {
            "google_data": data,
            "completed": state.get("completed", []) + ["google"],
        }
    return google_node


def make_comparably_node(llm):
    async def comparably_node(state: CompanyResearchState) -> dict:
        agent = ComparablyAgent(llm)
        data = await agent.research(
            state["company"], state["role"], state.get("location", "")
        )
        return {
            "comparably_data": data,
            "completed": state.get("completed", []) + ["comparably"],
        }
    return comparably_node


def make_levels_node(llm):
    async def levels_node(state: CompanyResearchState) -> dict:
        agent = LevelsAgent(llm)
        data = await agent.research(
            state["company"], state["role"], state.get("location", "")
        )
        return {
            "levels_data": data,
            "completed": state.get("completed", []) + ["levels"],
        }
    return levels_node


def make_repvue_node(llm):
    async def repvue_node(state: CompanyResearchState) -> dict:
        agent = RepVueAgent(llm)
        data = await agent.research(state["company"], state["role"])
        return {
            "repvue_data": data,
            "completed": state.get("completed", []) + ["repvue"],
        }
    return repvue_node


def make_compose_node(llm_streaming):
    async def compose_node(state: CompanyResearchState) -> dict:
        jd = (state.get("job_description") or "").strip()
        location = (state.get("location") or "").strip()
        resume = (state.get("tailored_resume") or "").strip()

        jd_section = (
            f"\n### Job Description (use this to make advice role-specific)\n{jd[:3000]}\n"
            if jd else ""
        )
        resume_section = (
            f"\n### Candidate's Tailored Resume (use this to predict gap questions and frame talking points)\n{resume[:3000]}\n"
            if resume else ""
        )
        location_note = f" ({location})" if location else ""

        def _section(label: str, data: str) -> str:
            return f"### {label}\n{data or 'Not available.'}\n" if data else ""

        prompt = f"""\
You are composing a detailed, role-specific interview preparation dossier.

Company: **{state['company']}**{location_note}
Target role: **{state['role']}**
{jd_section}{resume_section}
---
{_section("Glassdoor data", state.get("glassdoor_data"))}
{_section("Indeed data", state.get("indeed_data"))}
{_section("Google research", state.get("google_data"))}
{_section("Comparably data (culture, CEO rating, interview questions)", state.get("comparably_data"))}
{_section("Levels.fyi data (compensation by level)", state.get("levels_data"))}
{_section("RepVue data (sales team metrics, quota attainment)", state.get("repvue_data"))}
---

Write a structured dossier. Use the job description (if provided) to make every \
section specific to this exact role. Use markdown headers.

## 1. Company Overview
Culture, size, industry position, mission. CEO rating, employee sentiment, \
eNPS if available. What makes this company distinctive.

## 2. Compensation & Benefits
Salary / TC for **{state['role']}** specifically. Cross-reference Levels.fyi levels \
(base + equity + bonus breakdown), Comparably and Indeed ranges. \
For sales roles include OTE and quota attainment from RepVue. \
Equity, bonuses, benefits, perks.

## 3. Interview Process
Stages, typical timeline, format. Specific reported questions from Comparably \
and Glassdoor. If the JD mentions technologies or competencies, predict questions.

## 4. Work Environment
Work-life balance, remote/hybrid policy, management style, team culture. \
Comparably scores and Glassdoor reviews.

## 5. Strengths & Red Flags
What employees consistently praise and what they warn about.

## 6. Your Action Plan
Based on the JD requirements: 3 things to study or prepare, \
3 talking points to emphasise from your background, \
and 3 smart questions to ask the interviewer."""

        resp = await llm_streaming.ainvoke([HumanMessage(content=prompt)])
        return {"report": resp.content}
    return compose_node


# ── Graph assembly ────────────────────────────────────────────────────────────

def build_workflow(api_key: str, credentials: dict):
    nav_llm     = _make_llm(api_key)
    compose_llm = _make_llm(api_key, streaming=True)

    wf = StateGraph(CompanyResearchState)

    wf.add_node("supervisor",  supervisor_node)
    wf.add_node("glassdoor",   make_glassdoor_node(nav_llm))
    wf.add_node("indeed",      make_indeed_node(nav_llm))
    wf.add_node("google",      make_google_node(nav_llm))
    wf.add_node("comparably",  make_comparably_node(nav_llm))
    wf.add_node("levels",      make_levels_node(nav_llm))
    wf.add_node("repvue",      make_repvue_node(nav_llm))
    wf.add_node("compose",     make_compose_node(compose_llm))

    wf.set_entry_point("supervisor")

    wf.add_conditional_edges(
        "supervisor",
        lambda s: s["next_agent"],
        {
            "glassdoor":  "glassdoor",
            "indeed":     "indeed",
            "google":     "google",
            "comparably": "comparably",
            "levels":     "levels",
            "repvue":     "repvue",
            "compose":    "compose",
        },
    )

    for agent in _ALL_AGENTS:
        wf.add_edge(agent, "supervisor")
    wf.add_edge("compose", END)

    return wf.compile()
