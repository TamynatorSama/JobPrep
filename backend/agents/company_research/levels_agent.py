"""
Levels.fyi research agent.
Scrapes total compensation (TC) data by level, base/equity/bonus breakdown,
and location-adjusted pay ranges. No login required.
"""
import re
import json

from langchain_google_genai import ChatGoogleGenerativeAI

from .browser_agent import LLMBrowserAgent
from .browser_manager import new_context


def _company_slug(name: str) -> str:
    # Levels.fyi uses lowercase slugs with hyphens
    return re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")


class LevelsAgent:
    BASE = "https://www.levels.fyi"

    def __init__(self, llm: ChatGoogleGenerativeAI):
        self.llm = llm

    async def research(self, company: str, role: str, location: str = "") -> str:
        slug = _company_slug(company)
        ctx = await new_context()
        results: dict = {}

        try:
            # ── Company salary overview ───────────────────────────────────────
            page = await ctx.new_page()
            await page.goto(
                f"{self.BASE}/companies/{slug}/salaries/",
                wait_until="domcontentloaded",
                timeout=30000,
            )
            agent = LLMBrowserAgent(page, self.llm, max_steps=4)
            overview = await agent.run(
                f"Extract total compensation (TC) data from this Levels.fyi salary page "
                f"for '{company}'. "
                f"Focus on roles matching or close to '{role}'. "
                "For each level found (e.g. L3, L4, L5, Senior, Staff, Principal), collect: "
                "level name, median TC, base salary, equity (annualised), bonus, "
                "and number of data points. "
                f"If location '{location}' data is visible, note any location adjustments. "
                "Return a JSON object with keys: "
                "'levels' (list of {level, tc_median, base, equity, bonus, data_points}), "
                "'location_note' (string, if applicable)."
            )
            if overview:
                results["compensation_by_level"] = overview
            await page.close()

            # ── Role-specific search if direct page was sparse ────────────────
            page = await ctx.new_page()
            # Use the search to find the specific role
            role_slug = re.sub(r"[^a-z0-9]+", "-", role.lower()).strip("-")
            await page.goto(
                f"{self.BASE}/companies/{slug}/salaries/{role_slug}/",
                wait_until="domcontentloaded",
                timeout=30000,
            )
            agent = LLMBrowserAgent(page, self.llm, max_steps=3)
            role_data = await agent.run(
                f"Extract compensation details for '{role}' at '{company}' from this "
                "Levels.fyi page. Collect: median TC, base, equity, bonus, level breakdown, "
                "number of submissions, and any trend (increasing/decreasing). "
                "Return a JSON object with keys: role, tc_median, base, equity, "
                "bonus, submissions, trend, levels."
            )
            if role_data:
                results["role_compensation"] = role_data
            await page.close()

            if results:
                return json.dumps(results, indent=2)
            return f"Could not extract compensation data from Levels.fyi for '{company}'."

        except Exception as exc:
            return f"Levels.fyi agent error: {exc}"
        finally:
            await ctx.close()
