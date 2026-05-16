"""
RepVue research agent.
Scrapes sales team ratings, quota attainment data, OTE ranges, and culture
scores from RepVue. Most relevant for sales and revenue roles.
No login required for aggregate data.
"""
import re
import json

from langchain_google_genai import ChatGoogleGenerativeAI

from .browser_agent import LLMBrowserAgent
from .browser_manager import new_context


def _company_slug(name: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")


class RepVueAgent:
    BASE = "https://www.repvue.com"

    def __init__(self, llm: ChatGoogleGenerativeAI):
        self.llm = llm

    async def research(self, company: str, role: str) -> str:
        slug = _company_slug(company)
        ctx = await new_context()
        results: dict = {}

        try:
            # ── Company overview ──────────────────────────────────────────────
            page = await ctx.new_page()
            await page.goto(
                f"{self.BASE}/companies/{slug}/",
                wait_until="domcontentloaded",
                timeout=30000,
            )
            agent = LLMBrowserAgent(page, self.llm, max_steps=4)
            overview = await agent.run(
                f"Extract sales team data from this RepVue company page for '{company}'. "
                "Collect: overall RepVue score (out of 100), quota attainment % "
                "(percentage of reps hitting quota), culture score, "
                "product-market fit score, inbound lead flow score, "
                "leadership score, compensation & benefits score, "
                "number of ratings/reviews, and 2–3 highlighted rep quotes. "
                "Return a JSON object with keys: repvue_score, quota_attainment_pct, "
                "culture_score, pmf_score, inbound_score, leadership_score, "
                "comp_score, rating_count, quotes."
            )
            if overview:
                results["overview"] = overview
            await page.close()

            # ── Compensation (OTE) ────────────────────────────────────────────
            page = await ctx.new_page()
            await page.goto(
                f"{self.BASE}/companies/{slug}/salaries/",
                wait_until="domcontentloaded",
                timeout=30000,
            )
            agent = LLMBrowserAgent(page, self.llm, max_steps=3)
            comp = await agent.run(
                f"Extract compensation data from this RepVue salaries page for '{company}'. "
                f"Focus on roles matching '{role}' or similar sales/revenue titles. "
                "For each role collect: title, OTE (on-target earnings), "
                "base salary range, and variable/commission portion. "
                "Return a JSON object with key 'roles': "
                "[{title, ote, base, variable}]."
            )
            if comp:
                results["compensation"] = comp
            await page.close()

            if results:
                return json.dumps(results, indent=2)
            return f"Could not extract data from RepVue for '{company}'."

        except Exception as exc:
            return f"RepVue agent error: {exc}"
        finally:
            await ctx.close()
