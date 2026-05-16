import asyncio
import json

from fastapi import APIRouter
from sse_starlette.sse import EventSourceResponse

from models import ResearchRequest
from agents.workflow import build_research_workflow
from agents.state import ResearchState

router = APIRouter()

STAGE_LABELS = {
    "extract":   "\U0001f50d Analyzing job requirements...",
    "questions": "❓ Generating interview questions...",
    "tips":      "\U0001f4a1 Building your prep strategy...",
}


@router.post("/stream")
async def research_stream(req: ResearchRequest):
    async def generate():
        if not req.api_key:
            yield {"data": json.dumps({
                "type": "error",
                "content": "No Gemini API key set. Open **Settings** and paste your key.",
            })}
            return

        if len(req.job_description.strip()) < 30:
            yield {"data": json.dumps({
                "type": "error",
                "content": "Job description is too short to analyze meaningfully.",
            })}
            return

        try:
            workflow = build_research_workflow(req.api_key)

            initial_state: ResearchState = {
                "job_description": req.job_description,
                "company": req.company,
                "role": req.role,
                "requirements": "",
                "questions": [],
                "prep_tips": "",
            }

            async for event in workflow.astream_events(initial_state, version="v2"):
                event_type = event.get("event", "")
                node_name = event.get("name", "")

                if event_type == "on_chain_start" and node_name in STAGE_LABELS:
                    yield {"data": json.dumps({
                        "type": "stage",
                        "content": f"\n\n---\n{STAGE_LABELS[node_name]}\n\n",
                    })}
                    await asyncio.sleep(0)

                elif event_type == "on_chat_model_stream":
                    chunk = event["data"].get("chunk")
                    if chunk and hasattr(chunk, "content") and chunk.content:
                        yield {"data": json.dumps({"type": "token", "content": chunk.content})}
                        await asyncio.sleep(0)

            yield {"data": json.dumps({"type": "done"})}

        except Exception as exc:
            yield {"data": json.dumps({"type": "error", "content": str(exc)})}

    return EventSourceResponse(generate())
