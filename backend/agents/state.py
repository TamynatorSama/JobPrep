from typing import Annotated, TypedDict
from langchain_core.messages import BaseMessage
from langgraph.graph.message import add_messages


class ChatState(TypedDict):
    messages: Annotated[list[BaseMessage], add_messages]
    job_context: str


class ResearchState(TypedDict):
    job_description: str
    company: str
    role: str
    requirements: str
    questions: list[str]
    prep_tips: str
