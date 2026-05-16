"""
Indeed research agent.
Scrapes the public company page (reviews, salaries) and optionally logs in
to access more detailed employee reviews.
"""
import asyncio
import re

from langchain_google_genai import ChatGoogleGenerativeAI

from .browser_agent import LLMBrowserAgent
from .browser_manager import session_path, new_context


def _company_slug(name: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", name.lower()).strip("-")


class IndeedAgent:
    BASE = "https://www.indeed.com"

    def __init__(self, llm: ChatGoogleGenerativeAI, credentials: dict):
        self.llm = llm
        self.credentials = credentials  # {indeed_email, indeed_password}

    async def research(self, company: str, role: str) -> str:
        email = self.credentials.get("indeed_email", "")
        password = self.credentials.get("indeed_password", "")
        sp = session_path("indeed", email) if email else None

        slug = _company_slug(company)
        ctx = await new_context(storage_state_path=sp)
        results: dict = {}

        try:
            # ── Reviews ──────────────────────────────────────────────────────
            page = await ctx.new_page()
            reviews_url = f"{self.BASE}/cmp/{slug}/reviews"
            await page.goto(reviews_url, wait_until="domcontentloaded", timeout=30000)

            if email and password and await self._needs_login(page):
                await self._login(ctx, email, password, sp)
                page = await ctx.new_page()
                await page.goto(reviews_url, wait_until="domcontentloaded", timeout=30000)

            agent = LLMBrowserAgent(page, self.llm, max_steps=4)
            reviews_data = await agent.run(
                f"Extract from this Indeed company reviews page for '{company}': "
                "overall star rating, number of reviews, "
                "top pros mentioned in reviews, top cons mentioned, "
                "work-life balance score, compensation score, management score. "
                "Return a JSON object with keys: rating, review_count, pros, cons, "
                "work_life_balance, compensation, management."
            )
            if reviews_data:
                results["reviews"] = reviews_data
            await page.close()

            # ── Salaries ─────────────────────────────────────────────────────
            page = await ctx.new_page()
            salaries_url = f"{self.BASE}/cmp/{slug}/salaries"
            await page.goto(salaries_url, wait_until="domcontentloaded", timeout=30000)

            agent = LLMBrowserAgent(page, self.llm, max_steps=3)
            salary_data = await agent.run(
                f"Extract salary information from this Indeed salaries page for '{company}'. "
                f"Focus on roles similar to '{role}' if visible. "
                "List up to 5 roles with their salary ranges. "
                "Return a JSON object with key 'salaries': [{role, min, max, median, count}]."
            )
            if salary_data:
                results["salaries"] = salary_data
            await page.close()

            if sp:
                try:
                    await ctx.storage_state(path=sp)
                except Exception:
                    pass

            if results:
                import json
                return json.dumps(results, indent=2)
            return f"Could not extract data from Indeed for '{company}'."

        except Exception as exc:
            return f"Indeed agent error: {exc}"
        finally:
            await ctx.close()

    async def _needs_login(self, page) -> bool:
        try:
            content = await page.content()
            return "sign in" in content.lower() or "log in" in content.lower()
        except Exception:
            return False

    async def _login(self, ctx, email: str, password: str, sp: str | None):
        page = await ctx.new_page()
        try:
            await page.goto(
                "https://secure.indeed.com/auth",
                wait_until="domcontentloaded",
                timeout=30000,
            )
            agent = LLMBrowserAgent(page, self.llm, max_steps=10)
            await agent.run(
                f"Log in to Indeed with email '{email}' and password '{password}'. "
                "Indeed may ask for email first, then navigate to a password step. "
                "Complete the full login flow."
            )
            await asyncio.sleep(1.5)
            if sp:
                try:
                    await ctx.storage_state(path=sp)
                except Exception:
                    pass
        finally:
            await page.close()
