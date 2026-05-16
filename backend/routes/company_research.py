import asyncio
import json

from fastapi import APIRouter
from sse_starlette.sse import EventSourceResponse

from models import CompanyResearchRequest
from agents.company_research.orchestrator import build_workflow
from agents.company_research.state import CompanyResearchState

router = APIRouter()

# Maps LangGraph node names to user-friendly stage banners
_STAGE_BANNERS = {
    "supervisor":  "\n\n---\n🤖 **Coordinator** deciding next research step…\n\n",
    "glassdoor":   "\n\n---\n🔍 **Glassdoor** — scraping reviews, salary & interview data…\n\n",
    "indeed":      "\n\n---\n💼 **Indeed** — scraping company reviews & salary ranges…\n\n",
    "google":      "\n\n---\n🌐 **Google** — searching for culture, news & compensation data…\n\n",
    "comparably":  "\n\n---\n📊 **Comparably** — scraping culture scores, CEO ratings & interview questions…\n\n",
    "levels":      "\n\n---\n💰 **Levels.fyi** — scraping total compensation by level…\n\n",
    "repvue":      "\n\n---\n🎯 **RepVue** — scraping sales team metrics & quota attainment…\n\n",
    "compose":     "\n\n---\n📝 **Composing** your interview dossier…\n\n",
}


@router.post("/stream")
async def company_research_stream(req: CompanyResearchRequest):
    async def generate():
        if not req.api_key:
            yield {"data": json.dumps({
                "type": "error",
                "content": "No Gemini API key set. Open **Settings → API Keys**.",
            })}
            return

        if not req.company.strip():
            yield {"data": json.dumps({"type": "error", "content": "Company name is required."})}
            return

        try:
            credentials = {
                "glassdoor_email":    req.glassdoor_email or "",
                "glassdoor_password": req.glassdoor_password or "",
                "indeed_email":       req.indeed_email or "",
                "indeed_password":    req.indeed_password or "",
            }

            workflow = build_workflow(req.api_key, credentials)

            initial: CompanyResearchState = {
                "company":          req.company,
                "role":             req.role or "this role",
                "location":         req.location or "",
                "job_description":  req.job_description or "",
                "tailored_resume":  req.tailored_resume or "",
                "api_key":          req.api_key,
                "credentials":      credentials,
                "glassdoor_data":   "",
                "indeed_data":      "",
                "google_data":      "",
                "comparably_data":  "",
                "levels_data":      "",
                "repvue_data":      "",
                "completed":        [],
                "next_agent":       "",
                "report":           "",
                "errors":           [],
            }

            async for event in workflow.astream_events(initial, version="v2"):
                etype = event.get("event", "")
                name  = event.get("name", "")

                # Stage banners when a node starts
                if etype == "on_chain_start" and name in _STAGE_BANNERS:
                    yield {"data": json.dumps({
                        "type": "stage",
                        "content": _STAGE_BANNERS[name],
                    })}
                    await asyncio.sleep(0)

                # Token stream from any LLM call (supervisor reasoning + final compose)
                elif etype == "on_chat_model_stream":
                    chunk = event["data"].get("chunk")
                    if chunk and hasattr(chunk, "content") and chunk.content:
                        yield {"data": json.dumps({"type": "token", "content": chunk.content})}
                        await asyncio.sleep(0)

            yield {"data": json.dumps({"type": "done"})}

        except Exception as exc:
            yield {"data": json.dumps({"type": "error", "content": str(exc)})}

    return EventSourceResponse(generate())
