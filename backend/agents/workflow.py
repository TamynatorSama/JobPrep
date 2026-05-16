import functools

from langgraph.graph import StateGraph, END

from .state import ResearchState
from .nodes import extract_requirements, generate_questions, create_prep_tips


def build_research_workflow(api_key: str):
    workflow = StateGraph(ResearchState)

    workflow.add_node("extract", functools.partial(extract_requirements, api_key=api_key))
    workflow.add_node("questions", functools.partial(generate_questions, api_key=api_key))
    workflow.add_node("tips", functools.partial(create_prep_tips, api_key=api_key))

    workflow.set_entry_point("extract")
    workflow.add_edge("extract", "questions")
    workflow.add_edge("questions", "tips")
    workflow.add_edge("tips", END)

    return workflow.compile()
