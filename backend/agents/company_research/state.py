from typing import TypedDict


class CompanyResearchState(TypedDict):
    company: str
    role: str
    location: str              # office location — narrows Glassdoor/Indeed searches
    job_description: str       # full JD text — used by compose node for targeted advice
    tailored_resume: str       # tailored resume from the application-prep step
    api_key: str
    credentials: dict          # kept for backward compat (social login not supported)
    glassdoor_data: str
    indeed_data: str
    google_data: str
    comparably_data: str
    levels_data: str
    repvue_data: str
    completed: list[str]       # which agents have finished
    next_agent: str            # supervisor output
    report: str                # final composed report
    errors: list[str]
