import asyncio
import json
import sys
import traceback

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
        # Echo arrival so the frontend bubble updates immediately. Without
        # this, Playwright cold-start + supervisor LLM latency can swallow
        # 10–30s before any event reaches the client, which reads as a hang.
        print(
            f"[company-research] /stream hit company={req.company!r} "
            f"role={req.role!r} key_len={len(req.api_key or '')}",
            file=sys.stderr,
            flush=True,
        )
        yield {"data": json.dumps({
            "type": "stage",
            "content": "\n📡 **Request received** — building workflow…\n",
        })}
        await asyncio.sleep(0)

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

            yield {"data": json.dumps({
                "type": "stage",
                "content": "\n🚀 **Workflow built** — dispatching supervisor…\n",
            })}
            await asyncio.sleep(0)

            event_count = 0
            seen_nodes: set[str] = set()
            seen_names: set[str] = set()  # diagnostic: all on_chain_start names
            async for event in workflow.astream_events(initial, version="v2"):
                event_count += 1
                etype = event.get("event", "")
                name  = event.get("name", "")

                if etype == "on_chain_start" and name and name not in seen_names:
                    seen_names.add(name)
                    print(f"[company-research] on_chain_start name={name!r}",
                          file=sys.stderr, flush=True)

                # Banner when a node starts. Prefer the curated copy from
                # `_STAGE_BANNERS`; fall back to a generic banner so the user
                # still sees progress when LangGraph internals rename a node.
                if etype == "on_chain_start" and name and name not in seen_nodes:
                    if name in _STAGE_BANNERS:
                        seen_nodes.add(name)
                        yield {"data": json.dumps({
                            "type": "stage",
                            "content": _STAGE_BANNERS[name],
                        })}
                        await asyncio.sleep(0)
                    elif name.lower() in _STAGE_BANNERS:
                        seen_nodes.add(name)
                        yield {"data": json.dumps({
                            "type": "stage",
                            "content": _STAGE_BANNERS[name.lower()],
                        })}
                        await asyncio.sleep(0)

                # Final report — composer is the only node with streaming LLM,
                # so this only fires during the dossier write.
                elif etype == "on_chat_model_stream":
                    chunk = event["data"].get("chunk")
                    if chunk and hasattr(chunk, "content") and chunk.content:
                        yield {"data": json.dumps({"type": "token", "content": chunk.content})}
                        await asyncio.sleep(0)

                # Surface the composed report when the compose node finishes,
                # in case streaming events didn't carry tokens (fallback path).
                elif etype == "on_chain_end" and name == "compose":
                    output = event.get("data", {}).get("output") or {}
                    report = output.get("report") if isinstance(output, dict) else None
                    if report:
                        yield {"data": json.dumps({"type": "token", "content": report})}
                        await asyncio.sleep(0)

            print(
                f"[company-research] stream done, event_count={event_count}, "
                f"node_banners_emitted={sorted(seen_nodes)}",
                file=sys.stderr,
                flush=True,
            )
            yield {"data": json.dumps({"type": "done"})}

        except Exception as exc:
            tb = traceback.format_exc()
            print(f"[company-research] FATAL: {exc}\n{tb}", file=sys.stderr, flush=True)
            yield {"data": json.dumps({
                "type": "error",
                "content": f"{type(exc).__name__}: {exc}",
            })}

    return EventSourceResponse(generate())
