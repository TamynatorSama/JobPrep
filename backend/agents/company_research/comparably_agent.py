"""
Comparably research agent.
Scrapes culture scores, CEO ratings, salary data, and interview questions
from Comparably (no login required for most data).

Comparably uses PerimeterX bot protection. We detect challenge pages early
so the LLM loop doesn't waste calls trying to defeat them.
"""
import re
import json

from langchain_google_genai import ChatGoogleGenerativeAI

from .browser_agent import LLMBrowserAgent, detect_block
from .browser_manager import new_context


def _company_slug(name: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")


class ComparablyAgent:
    BASE = "https://www.comparably.com"

    def __init__(self, llm: ChatGoogleGenerativeAI):
        self.llm = llm

    async def _scrape_section(
        self,
        ctx,
        url: str,
        section_key: str,
        goal: str,
    ) -> dict | str | None:
        """Open a Comparably page; return extracted data, a 'blocked' marker, or None."""
        page = await ctx.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=30000)
            block = await detect_block(page)
            if block:
                return {"blocked": block, "url": url}
            agent = LLMBrowserAgent(
                page, self.llm, max_steps=3, trace_label=f"comparably_{section_key}"
            )
            return await agent.run(goal)
        finally:
            await page.close()

    async def research(self, company: str, role: str, location: str = "") -> str:
        slug = _company_slug(company)
        ctx = await new_context()
        results: dict = {}

        try:
            loc_hint = f" Prefer data tagged with the '{location}' office if shown." if location else ""
            overview = await self._scrape_section(
                ctx,
                f"{self.BASE}/companies/{slug}/",
                "overview",
                f"Extract from this Comparably company page for '{company}': "
                "overall company grade/score (A+/A/B etc.), CEO rating %, "
                "culture score, work-life balance score, compensation score, "
                "perks & benefits score, professional development score, "
                "eNPS (employee net promoter score) if visible, "
                "number of ratings, and the top 2–3 highlighted strengths or quotes."
                f"{loc_hint} "
                "Return a JSON object with keys: grade, ceo_rating, culture_score, "
                "work_life_balance, compensation_score, perks_score, "
                "dev_score, enps, rating_count, highlights.",
            )
            if overview:
                results["overview"] = overview

            # If the overview was blocked, the same anti-bot system will block
            # every other page on the domain — bail out fast.
            if isinstance(overview, dict) and overview.get("blocked"):
                return json.dumps(
                    {
                        "blocked": True,
                        "reason": overview["blocked"],
                        "note": "Comparably's bot protection (PerimeterX) refused "
                                "the headless request. Public Google snippets will "
                                "cover the same data via the google agent.",
                    },
                    indent=2,
                )

            salaries = await self._scrape_section(
                ctx,
                f"{self.BASE}/companies/{slug}/salaries/",
                "salaries",
                f"Extract salary data from this Comparably salaries page for '{company}'. "
                f"Focus on roles similar to '{role}'. "
                "List up to 6 roles with their annual salary ranges. "
                "Return a JSON object with key 'salaries': "
                "[{role, median, low, high}].",
            )
            if salaries:
                results["salaries"] = salaries

            interviews = await self._scrape_section(
                ctx,
                f"{self.BASE}/companies/{slug}/interviews/",
                "interviews",
                f"Extract interview information from this Comparably interviews page "
                f"for '{company}'. Collect: interview difficulty rating, "
                f"process description, and up to 6 actual interview questions "
                f"(especially any relevant to a '{role}' position). "
                "Return a JSON object with keys: difficulty, process, questions (list).",
            )
            if interviews:
                results["interviews"] = interviews

            if results:
                return json.dumps(results, indent=2)
            return f"Could not extract data from Comparably for '{company}'."

        except Exception as exc:
            return f"Comparably agent error: {exc}"
        finally:
            await ctx.close()
