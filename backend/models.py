from pydantic import BaseModel
from typing import List, Literal, Optional


class ChatMessage(BaseModel):
    """One turn in a conversation. `role` matches the OpenAI/Gemini convention."""
    role: Literal["user", "assistant"]
    content: str


class LLMConfig(BaseModel):
    """Which LLM powers a request, plus the keys for every provider the user
    has configured. The Rust shell builds this from Windows Credential Manager
    and attaches it to every request; `provider` is the Settings toggle.
    Spare keys ride along so company research can use them as fallback lanes
    and RAG can pick an embeddings-capable provider."""
    provider: str = "gemini"      # gemini | openai | anthropic
    model: str = ""               # optional model override for the selected provider
    gemini_api_key: str = ""
    openai_api_key: str = ""
    anthropic_api_key: str = ""


class RagDoc(BaseModel):
    """One retrievable document from a job's corpus. `source` is a short label
    (e.g. "resume", "company_research", "chat: Mock Interview") surfaced to the
    LLM alongside the retrieved text."""
    source: str
    text: str


class ChatRequest(BaseModel):
    message: str
    job_context: Optional[str] = ""
    llm: LLMConfig = LLMConfig()
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
    llm: LLMConfig = LLMConfig()


class CompanyResearchRequest(BaseModel):
    company: str
    role: Optional[str] = ""
    location: Optional[str] = ""          # narrows site searches to the right office
    job_description: Optional[str] = ""   # used by compose node for role-specific advice
    tailored_resume: Optional[str] = ""   # output of the application-prep step
    llm: LLMConfig = LLMConfig()


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
    llm: LLMConfig = LLMConfig()


class CheatsheetRequest(BaseModel):
    """Build / refresh a job's interview cheatsheet.

    Aggregates everything known about a job — JD, the candidate's resume, the
    company-research dossier, and prior conversations (coach chats + mock
    interviews) — into a structured cheatsheet (STAR stories, key facts,
    questions to ask) plus a maintained markdown doc. `previous_markdown` lets
    the LLM refine the existing sheet instead of regenerating from scratch.
    """
    company: str
    role: str
    location: Optional[str] = ""
    job_description: Optional[str] = ""
    resume: Optional[str] = ""            # tailored or master resume text
    company_research: Optional[str] = ""  # research dossier text
    documents: List[RagDoc] = []          # conversation transcripts (chats / interviews)
    previous_markdown: Optional[str] = "" # existing cheatsheet to refine, if any
    llm: LLMConfig = LLMConfig()


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
    llm: LLMConfig = LLMConfig()


# ── Browser-extension bridge ────────────────────────────────────────────────

class SeedConfigRequest(BaseModel):
    """Refresh the in-memory LLM config (provider toggle + all keys).
    Token-guarded; called by the Rust shell on startup and whenever the user
    saves Settings, so the browser-extension endpoints pick up changes without
    an app restart."""
    llm: LLMConfig = LLMConfig()


class FieldSpec(BaseModel):
    """One form field scraped from an application page by the extension."""
    key: str                              # stable id the extension maps answers back to
    label: str                            # human label / question shown next to the field
    type: str = "text"                    # text | textarea | select | radio | checkbox | email | tel | ...
    options: List[str] = []               # for select / radio: the choosable option labels


class AnswerFieldsRequest(BaseModel):
    """Ask the backend to answer scraped application-form fields from the
    candidate's resume + the job context."""
    job_id: Optional[str] = ""            # picks JD/company/role from the store
    resume_ids: List[int] = []            # which master resumes; empty = all
    fields: List[FieldSpec]
    # Optional direct overrides if the extension already has the context.
    company: Optional[str] = ""
    role: Optional[str] = ""
    job_description: Optional[str] = ""


class RememberRequest(BaseModel):
    """Persist a user-supplied answer into the reusable answer bank."""
    label: str
    value: str


class JobCaptureRequest(BaseModel):
    """A JD captured by the extension, queued for the app to turn into a job +
    company-research run."""
    company: str = ""
    role: str = ""
    location: str = ""
    url: str = ""
    job_description: str = ""
    # Display metadata parsed at capture time (JSON-LD or AI). Optional — the app
    # creates the job fine without them; they enrich the record when present.
    employment_type: str = ""
    work_mode: str = ""
    salary: str = ""
    posted: str = ""
    requirements: List[str] = []


class InboxAckRequest(BaseModel):
    """Ids the app has consumed from the capture inbox."""
    ids: List[str] = []


class TimelineEventRequest(BaseModel):
    """Queued by the extension after an autofill finishes — the app applies it to
    the job's stage timeline (e.g. mark Applied + a note)."""
    job_id: str = ""
    status: str = "Applied"
    note: str = ""


class PageExtractRequest(BaseModel):
    """Raw page content for the LLM to parse a JD out of when heuristics fail."""
    url: str = ""
    title: str = ""
    page_text: str = ""
