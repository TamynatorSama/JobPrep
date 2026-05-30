from pydantic import BaseModel
from typing import List, Literal, Optional


class ChatMessage(BaseModel):
    """One turn in a conversation. `role` matches the OpenAI/Gemini convention."""
    role: Literal["user", "assistant"]
    content: str


class RagDoc(BaseModel):
    """One retrievable document from a job's corpus. `source` is a short label
    (e.g. "resume", "company_research", "chat: Mock Interview") surfaced to the
    LLM alongside the retrieved text."""
    source: str
    text: str


class ChatRequest(BaseModel):
    message: str
    job_context: Optional[str] = ""
    api_key: Optional[str] = ""
    # Prior conversation turns, oldest first. Empty for the first message.
    history: List[ChatMessage] = []
    # "coach" (default, friendly assistant) or "interviewer" (in-character live interview).
    mode: Literal["coach", "interviewer"] = "coach"
    # The job's corpus (resume, company research, sibling chats) for RAG. The
    # backend embeds + retrieves the most relevant chunks per message. Empty
    # disables retrieval.
    documents: List[RagDoc] = []


class ResearchRequest(BaseModel):
    job_description: str
    company: str
    role: str
    api_key: Optional[str] = ""


class CompanyResearchRequest(BaseModel):
    company: str
    role: Optional[str] = ""
    location: Optional[str] = ""          # narrows site searches to the right office
    job_description: Optional[str] = ""   # used by compose node for role-specific advice
    tailored_resume: Optional[str] = ""   # output of the application-prep step
    api_key: Optional[str] = ""
    glassdoor_email: Optional[str] = ""
    glassdoor_password: Optional[str] = ""
    indeed_email: Optional[str] = ""
    indeed_password: Optional[str] = ""


class MasterResume(BaseModel):
    name: str
    text: str
    # Base64-encoded raw .docx bytes, when the resume was uploaded as a .docx.
    # Used as the styling base for in-place section editing so the tailored
    # output preserves the original document's fonts, headings, and layout.
    docx_b64: Optional[str] = None


class ApplicationRequest(BaseModel):
    """Tailor a resume + cover letter for a specific job.

    The client (Rust app) sends every master resume it has stored; the LLM
    picks the closest match by role title / responsibilities and tailors it.
    """
    company: str
    role: str
    location: Optional[str] = ""
    job_description: str
    master_resumes: List[MasterResume]
    api_key: str


class KnockoutRequest(BaseModel):
    """Simulate a recruiter's phone-screen / knockout interview.

    Uses the JD + the candidate's tailored resume to predict the 6-8 questions
    a recruiter would ask first to filter the candidate in or out, plus the
    suggested answer constructed from the resume's evidence.
    """
    company: str
    role: str
    location: Optional[str] = ""
    job_description: str
    tailored_resume: str
    api_key: str
