"""
Interactive test runner for the company-research agents.

Examples
--------
  # default: run one agent (comparably) against Stripe + Software Engineer
  python test_research.py

  # specific company / role / location
  python test_research.py --company "Anthropic" --role "ML Engineer" --location "San Francisco"

  # run a different single agent
  python test_research.py --company "Stripe" --agent levels

  # run every site agent sequentially
  python test_research.py --company "Stripe" --agent all

  # run the full LangGraph workflow (supervisor → all agents → compose)
  python test_research.py --company "Stripe" --role "Software Engineer" --workflow

Each run creates a timestamped folder under backend/test_outputs/
containing per-agent traces:
    <agent>/result.json                   final extracted JSON
    <agent>/NN_stepMM_snapshot.txt        page state seen by the LLM
    <agent>/NN_stepMM_action.txt          action the LLM chose
    <agent>/NN_stepMM_result.txt          extracted payload (on "done")
NN = monotonic agent counter, MM = step within that agent.
"""
import argparse
import asyncio
import json
import os
import sys
import time
from datetime import datetime
from pathlib import Path

# Allow `python test_research.py` from the backend/ directory
sys.path.insert(0, str(Path(__file__).parent))

# ── ANSI colors (Windows Terminal / PowerShell 7 handle these fine) ─────────
R = "\033[0m"
B = "\033[1m"
BLUE    = "\033[94m"
GREEN   = "\033[92m"
YELLOW  = "\033[93m"
MAGENTA = "\033[95m"
CYAN    = "\033[96m"
GRAY    = "\033[90m"

def banner(text: str, color: str = CYAN) -> None:
    bar = "─" * 72
    print(f"\n{color}{bar}\n  {B}{text}{R}{color}\n{bar}{R}")

def info(label: str, value: str, color: str = BLUE) -> None:
    print(f"  {color}{label:10}{R} {value}")

# ── Order matters: 'all' iterates this list ─────────────────────────────────
AGENTS = ["glassdoor", "indeed", "google", "comparably", "levels", "repvue"]


async def run_one_agent(name: str, company: str, role: str, location: str,
                        llm, trace_dir: Path) -> str:
    """Run a single agent end-to-end. Returns raw output text (JSON or error)."""
    banner(f"AGENT: {name}", MAGENTA)

    agent_dir = trace_dir / name
    agent_dir.mkdir(parents=True, exist_ok=True)
    os.environ["BROWSER_AGENT_TRACE_DIR"] = str(agent_dir)

    t0 = time.time()
    try:
        if name == "glassdoor":
            from agents.company_research.glassdoor_agent import GlassdoorAgent
            data = await GlassdoorAgent(llm, {}).research(company, role, location)
        elif name == "indeed":
            from agents.company_research.indeed_agent import IndeedAgent
            data = await IndeedAgent(llm, {}).research(company, role)
        elif name == "google":
            from agents.company_research.google_agent import GoogleAgent
            data = await GoogleAgent(llm).research(company, role, location)
        elif name == "comparably":
            from agents.company_research.comparably_agent import ComparablyAgent
            data = await ComparablyAgent(llm).research(company, role, location)
        elif name == "levels":
            from agents.company_research.levels_agent import LevelsAgent
            data = await LevelsAgent(llm).research(company, role, location)
        elif name == "repvue":
            from agents.company_research.repvue_agent import RepVueAgent
            data = await RepVueAgent(llm).research(company, role)
        else:
            raise ValueError(f"unknown agent: {name}")
    except Exception as exc:
        elapsed = time.time() - t0
        print(f"\n{YELLOW}  ✗ {name} crashed after {elapsed:.1f}s: {exc}{R}")
        (agent_dir / "result.json").write_text(
            json.dumps({"error": str(exc)}, indent=2), encoding="utf-8"
        )
        return ""

    elapsed = time.time() - t0

    # Pretty-print the result. Most agents return a JSON string.
    result_file = agent_dir / "result.json"
    try:
        parsed = json.loads(data) if isinstance(data, str) else data
        result_file.write_text(json.dumps(parsed, indent=2, ensure_ascii=False), encoding="utf-8")
        size = len(json.dumps(parsed))
        keys = list(parsed.keys()) if isinstance(parsed, dict) else []
        print(f"\n{GREEN}  ✓ {name} done in {elapsed:.1f}s — {size} bytes, keys={keys}{R}")
        print(f"  {GRAY}↳ {result_file}{R}")
    except (json.JSONDecodeError, TypeError):
        result_file.write_text(str(data), encoding="utf-8")
        print(f"\n{YELLOW}  ✓ {name} done in {elapsed:.1f}s — non-JSON output ({len(str(data))} chars){R}")
        print(f"  {GRAY}↳ {result_file}{R}")

    return data


async def run_workflow(company: str, role: str, location: str, jd: str,
                       api_key: str, trace_dir: Path) -> None:
    """Run the full LangGraph supervisor workflow with per-node trace dirs.

    For every node start event we swap BROWSER_AGENT_TRACE_DIR so the
    LLMBrowserAgent instances inside that node write into their own folder
    (e.g. node_glassdoor/, node_indeed/). The supervisor's choices and each
    agent's extracted payload are printed inline so you can follow the flow.
    """
    banner(f"FULL WORKFLOW — {company} · {role}", CYAN)

    from agents.company_research.orchestrator import build_workflow
    from agents.company_research.state import CompanyResearchState

    workflow = build_workflow(api_key, {})

    initial: CompanyResearchState = {
        "company": company, "role": role, "location": location,
        "job_description": jd, "api_key": api_key, "credentials": {},
        "glassdoor_data": "", "indeed_data": "", "google_data": "",
        "comparably_data": "", "levels_data": "", "repvue_data": "",
        "completed": [], "next_agent": "", "report": "", "errors": [],
    }

    AGENT_NODES   = {"glassdoor", "indeed", "google", "comparably", "levels", "repvue"}
    SPECIAL_NODES = {"supervisor", "compose"}
    NODE_NAMES    = AGENT_NODES | SPECIAL_NODES
    DATA_KEY = {n: f"{n}_data" for n in AGENT_NODES}

    # Default trace dir for anything that runs before the first agent node
    os.environ["BROWSER_AGENT_TRACE_DIR"] = str(trace_dir / "_misc")

    t0 = time.time()
    report_chunks: list[str] = []
    composing = False
    node_t0: dict[str, float] = {}
    node_summary: list[tuple[str, float, int, str]] = []

    async for event in workflow.astream_events(initial, version="v2"):
        etype = event.get("event", "")
        name  = event.get("name", "")

        # ── Node start ────────────────────────────────────────────────────
        if etype == "on_chain_start" and name in NODE_NAMES:
            node_t0[name] = time.time()

            if name in AGENT_NODES:
                agent_dir = trace_dir / f"node_{name}"
                agent_dir.mkdir(parents=True, exist_ok=True)
                os.environ["BROWSER_AGENT_TRACE_DIR"] = str(agent_dir)
                banner(f"▶ NODE: {name}   (traces → {agent_dir.name}/)", BLUE)
            elif name == "supervisor":
                print(f"\n{GRAY}─── supervisor deciding next step ───{R}")
            elif name == "compose":
                composing = True
                banner("▶ NODE: compose  (streaming final report)", MAGENTA)

        # ── Node end ──────────────────────────────────────────────────────
        elif etype == "on_chain_end" and name in NODE_NAMES:
            elapsed = time.time() - node_t0.get(name, time.time())
            output = (event.get("data") or {}).get("output") or {}

            if name in AGENT_NODES:
                extracted = output.get(DATA_KEY[name], "") if isinstance(output, dict) else ""
                size = len(extracted) if isinstance(extracted, str) else 0

                # Detect blocked / weak / good
                blocked = isinstance(extracted, str) and '"blocked": true' in extracted.lower()
                weak    = size < 120 or (isinstance(extracted, str) and extracted.startswith(("Could not", f"{name.title()} agent error")))
                tag, status = (
                    (f"{YELLOW}BLOCKED{R}", "blocked") if blocked else
                    (f"{YELLOW}WEAK   {R}", "weak")    if weak else
                    (f"{GREEN}OK     {R}",  "ok")
                )
                print(f"\n  {tag} {name:11} {elapsed:5.1f}s  {size:6}b")
                node_summary.append((name, elapsed, size, status))

                # Persist the agent's raw return value alongside its browser traces
                (trace_dir / f"node_{name}" / "result.txt").write_text(
                    extracted if isinstance(extracted, str) else json.dumps(extracted, indent=2),
                    encoding="utf-8",
                )
            elif name == "supervisor":
                chosen = output.get("next_agent", "?") if isinstance(output, dict) else "?"
                arrow_color = MAGENTA if chosen == "compose" else CYAN
                print(f"  → supervisor chose: {arrow_color}{B}{chosen}{R}  ({elapsed:.1f}s)")
            elif name == "compose":
                print(f"\n\n{GREEN}✓ compose done in {elapsed:.1f}s{R}")

        # ── LLM tokens (compose body) ─────────────────────────────────────
        elif etype == "on_chat_model_stream":
            chunk = event["data"].get("chunk")
            if chunk and hasattr(chunk, "content") and chunk.content:
                if composing:
                    print(chunk.content, end="", flush=True)
                report_chunks.append(chunk.content)

    elapsed = time.time() - t0
    full = "".join(report_chunks)

    report_file = trace_dir / "final_report.md"
    report_file.write_text(full, encoding="utf-8")

    # ── Final summary table ──────────────────────────────────────────────
    banner(f"WORKFLOW COMPLETE — {elapsed:.1f}s", GREEN)
    print(f"  {B}{'AGENT':12} {'TIME':>7}  {'SIZE':>8}  STATUS{R}")
    for nm, el, sz, st in node_summary:
        color = GREEN if st == "ok" else YELLOW
        print(f"  {nm:12} {el:6.1f}s  {sz:7}b  {color}{st}{R}")
    print(f"\n  Report:  {GRAY}{report_file}{R}")
    print(f"  Traces:  {GRAY}{trace_dir}{R}\n")


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--company",  default="Stripe")
    parser.add_argument("--role",     default="Software Engineer")
    parser.add_argument("--location", default="")
    parser.add_argument("--jd",       default="", help="Job description text (used by --workflow compose)")
    parser.add_argument("--agent",    default="comparably",
                        choices=AGENTS + ["all"],
                        help="Single agent to test, or 'all' for every site")
    parser.add_argument("--workflow", action="store_true",
                        help="Run the LangGraph supervisor workflow instead of single agents")
    parser.add_argument("--api-key",  default=os.environ.get("GEMINI_API_KEY", ""),
                        help="Gemini API key (defaults to $GEMINI_API_KEY)")
    parser.add_argument("--quiet",    action="store_true",
                        help="Suppress per-step LLM trace prints (files are still written)")
    args = parser.parse_args()

    if not args.api_key:
        print(f"{YELLOW}Set GEMINI_API_KEY env var or pass --api-key{R}")
        sys.exit(2)

    # Enable per-step prints unless --quiet
    os.environ["BROWSER_AGENT_VERBOSE"] = "0" if args.quiet else "1"

    ts = datetime.now().strftime("%Y%m%d_%H%M%S")
    safe = "".join(c if c.isalnum() else "-" for c in args.company.lower()).strip("-")
    trace_dir = Path(__file__).parent / "test_outputs" / f"{ts}_{safe}"
    trace_dir.mkdir(parents=True, exist_ok=True)

    banner("InterPrep — Research Test Runner")
    info("company",  args.company)
    info("role",     args.role)
    info("location", args.location or "(none)")
    info("agent",    "FULL WORKFLOW" if args.workflow else args.agent)
    info("trace",    str(trace_dir), GRAY)

    try:
        if args.workflow:
            await run_workflow(args.company, args.role, args.location, args.jd,
                               args.api_key, trace_dir)
        else:
            from langchain_google_genai import ChatGoogleGenerativeAI
            llm = ChatGoogleGenerativeAI(model="gemini-2.5-flash",
                                         google_api_key=args.api_key)

            targets = AGENTS if args.agent == "all" else [args.agent]
            results: dict[str, str] = {}
            for name in targets:
                results[name] = await run_one_agent(
                    name, args.company, args.role, args.location, llm, trace_dir
                )

            banner("SUMMARY", CYAN)
            for name, data in results.items():
                size = len(data) if data else 0
                ok = size > 100 and not data.startswith(("Could not", "Glassdoor agent error",
                                                          "Indeed agent error", "Google agent error",
                                                          "Comparably agent error", "Levels.fyi agent error",
                                                          "RepVue agent error"))
                tag = f"{GREEN}OK   {R}" if ok else f"{YELLOW}WEAK {R}"
                print(f"  {tag} {name:12} {size:6}b  →  {trace_dir / name / 'result.json'}")
            print(f"\n  All traces: {GRAY}{trace_dir}{R}")
    finally:
        # Close the shared Playwright browser cleanly
        try:
            from agents.company_research.browser_manager import shutdown
            await shutdown()
        except Exception:
            pass


if __name__ == "__main__":
    asyncio.run(main())
