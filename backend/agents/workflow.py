import functools

from langgraph.graph import StateGraph, END

from models import LLMConfig

from .state import ResearchState
from .nodes import extract_requirements, generate_questions, create_prep_tips


def build_research_workflow(cfg: LLMConfig):
    workflow = StateGraph(ResearchState)

    workflow.add_node("extract", functools.partial(extract_requirements, cfg=cfg))
    workflow.add_node("questions", functools.partial(generate_questions, cfg=cfg))
    workflow.add_node("tips", functools.partial(create_prep_tips, cfg=cfg))

    workflow.set_entry_point("extract")
    workflow.add_edge("extract", "questions")
    workflow.add_edge("questions", "tips")
    workflow.add_edge("tips", END)

    return workflow.compile()
