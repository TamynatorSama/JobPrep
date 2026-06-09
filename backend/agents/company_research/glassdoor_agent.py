"""
Glassdoor research agent.
Scrapes public company data; optionally logs in with stored credentials
to unlock reviews and salary data behind the login wall.
Sessions are persisted to disk so login is only required once.
"""
import asyncio

from langchain_google_genai import ChatGoogleGenerativeAI
from playwright.async_api import BrowserContext

from .browser_agent import LLMBrowserAgent
from .browser_manager import session_path, new_context, save_storage_state


class GlassdoorAgent:
    BASE = "https://www.glassdoor.com"

    def __init__(self, llm: ChatGoogleGenerativeAI, credentials: dict):
        self.llm = llm
        self.credentials = credentials  # {glassdoor_email, glassdoor_password}

    async def research(self, company: str, role: str, location: str = "") -> str:
        email = self.credentials.get("glassdoor_email", "")
        password = self.credentials.get("glassdoor_password", "")
        sp = session_path("glassdoor", email) if email else None

        loc_param = f"&locKeyword={location.replace(' ', '+')}" if location else ""
        ctx: BrowserContext = await new_context(storage_state_path=sp)
        try:
            page = await ctx.new_page()
            await page.goto(
                f"{self.BASE}/Search/results.htm?keyword={company.replace(' ', '+')}{loc_param}",
                wait_until="domcontentloaded",
                timeout=30000,
            )

            agent = LLMBrowserAgent(page, self.llm, max_steps=6)

            # Step 1: navigate to the company overview page
            await agent.run(
                f"Find '{company}' in the search results and click on it to open "
                "the company overview page. Look for a company name link."
            )

            # Step 2: if login wall appears and we have credentials, log in
            if email and password and await self._login_wall_visible(page):
                await self._login(ctx, email, password, sp)
                # Re-open the company page after login
                page = await ctx.new_page()
                await page.goto(
                    f"{self.BASE}/Search/results.htm?keyword={company.replace(' ', '+')}",
                    wait_until="domcontentloaded",
                    timeout=30000,
                )
                agent = LLMBrowserAgent(page, self.llm, max_steps=6)
                await agent.run(
                    f"Find '{company}' in the search results and click on it."
                )

            # Step 3: extract what we can see
            extractor = LLMBrowserAgent(page, self.llm, max_steps=3)
            data = await extractor.run(
                f"Extract as much as possible from this Glassdoor company page for '{company}'. "
                f"Pay special attention to data relevant to a '{role}' position. "
                "Collect: overall rating (out of 5), CEO approval %, would recommend %, "
                "work-life balance rating, culture & values rating, "
                f"salary range for '{role}' or similar titles, interview difficulty, "
                "summary of latest 3-5 reviews (pros/cons), "
                "and any interview questions visible. "
                "Return as a JSON object with keys: rating, recommend_pct, "
                "work_life_balance, salaries, interview_difficulty, reviews, interview_questions."
            )

            # Persist session after successful navigation
            if sp:
                await save_storage_state(ctx, sp)

            await page.close()

            if data:
                import json
                return json.dumps(data, indent=2)
            return "Could not extract structured data. Glassdoor may have blocked the request."

        except Exception as exc:
            return f"Glassdoor agent error: {exc}"
        finally:
            await ctx.close()

    async def _login_wall_visible(self, page) -> bool:
        try:
            content = await page.content()
            keywords = ["sign-in", "sign in", "log in", "login", "create an account"]
            return any(k in content.lower() for k in keywords)
        except Exception:
            return False

    async def _login(self, ctx: BrowserContext, email: str, password: str, sp: str | None):
        """Log in to Glassdoor. Uses LLM to handle the login form."""
        page = await ctx.new_page()
        try:
            await page.goto(
                f"{self.BASE}/profile/login_input.htm",
                wait_until="domcontentloaded",
                timeout=30000,
            )
            agent = LLMBrowserAgent(page, self.llm, max_steps=8)
            await agent.run(
                f"Log in to Glassdoor. The email is '{email}' and the password is '{password}'. "
                "Fill the email field, then the password field, then click Sign In. "
                "If asked about cookies, accept them first."
            )
            await asyncio.sleep(1.5)
            if sp:
                await save_storage_state(ctx, sp)
        finally:
            await page.close()
