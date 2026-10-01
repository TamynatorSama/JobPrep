from langchain_core.messages import HumanMessage

import llm_provider as llm_factory
from models import LLMConfig

from .state import ResearchState


def make_llm(cfg: LLMConfig):
    return llm_factory.make_chat_model(cfg, tier="fast")


def llm_model_name(llm) -> str:
    """Model id of a LangChain chat model, for the token meter."""
    return str(getattr(llm, "model", "") or getattr(llm, "model_name", "") or "unknown")


async def extract_requirements(state: ResearchState, cfg: LLMConfig) -> ResearchState:
    llm = make_llm(cfg)
    prompt = f"""Extract the key requirements from this job posting. Format your response as:

**Must-Have Skills:** list the hard requirements
**Nice-to-Have:** list preferred but optional skills
**Experience Level:** junior / mid / senior / staff
**Culture Indicators:** what this company values (speed, process, collaboration, etc.)

Company: {state['company']}
Role: {state['role']}

Job Description:
{state['job_description'][:3000]}"""

    response = await llm.ainvoke([HumanMessage(content=prompt)])
    llm_factory.record_usage(llm_model_name(llm), getattr(response, "usage_metadata", None))
    return {**state, "requirements": llm_factory.content_text(response)}


async def generate_questions(state: ResearchState, cfg: LLMConfig) -> ResearchState:
    llm = make_llm(cfg)
    prompt = f"""Based on these job requirements, generate 8 likely interview questions:
- 4 behavioral questions (expect STAR format answers)
- 4 technical/role-specific questions

Requirements:
{state['requirements']}

Role: {state['role']} at {state['company']}

Number each question. Be specific to this role, not generic."""

    response = await llm.ainvoke([HumanMessage(content=prompt)])
    llm_factory.record_usage(llm_model_name(llm), getattr(response, "usage_metadata", None))
    # content_text: Gemini 3.x / Claude can return a list of content blocks.
    text = llm_factory.content_text(response)
    lines = text.split("\n")
    questions = [l.strip() for l in lines if l.strip() and l.strip()[0].isdigit()]
    return {**state, "questions": questions or [text]}


async def create_prep_tips(state: ResearchState, cfg: LLMConfig) -> ResearchState:
    llm = make_llm(cfg)
    questions_text = "\n".join(state.get("questions", []))
    prompt = f"""Create a targeted preparation strategy for this interview:

Role: {state['role']} at {state['company']}

Key Requirements:
{state['requirements'][:800]}

Likely Interview Questions:
{questions_text[:600]}

Provide:
1. **Top 3 things to prepare** (specific study topics or talking points)
2. **Your strongest angles** (what to emphasize from your background)
3. **Questions to ask them** (3 smart questions that show research)
4. **Potential red flags** (gaps or weaknesses to address proactively)

Keep it specific and actionable."""

    response = await llm.ainvoke([HumanMessage(content=prompt)])
    llm_factory.record_usage(llm_model_name(llm), getattr(response, "usage_metadata", None))
    return {**state, "prep_tips": llm_factory.content_text(response)}
