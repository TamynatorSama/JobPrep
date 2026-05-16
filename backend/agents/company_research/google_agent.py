"""
Google research agent.

We do NOT scrape google.com — Search has the web's most aggressive bot
detection and trips the "unusual traffic" CAPTCHA almost immediately,
locking the IP out for hours.

Instead we use Gemini 2.x's native Google Search grounding tool via the
google-genai SDK (installed as a transitive dep of langchain-google-genai).
Search is called through the official API path, no Playwright, no CAPTCHA.
"""
import json
import re

from langchain_google_genai import ChatGoogleGenerativeAI

try:
    from google import genai
    from google.genai import types as genai_types
    _HAS_GENAI = True
except ImportError:
    _HAS_GENAI = False


def _strip_fences(text: str) -> str:
    text = re.sub(r"^```(?:json)?\s*", "", text.strip(), flags=re.MULTILINE)
    text = re.sub(r"\s*```$", "", text, flags=re.MULTILINE)
    return text.strip()


class GoogleAgent:
    """Researches a company via Gemini's Google Search grounding tool."""

    def __init__(self, llm: ChatGoogleGenerativeAI):
        self.llm = llm
        # langchain-google-genai stores the key as a Pydantic SecretStr
        key = getattr(llm, "google_api_key", None)
        self.api_key = (
            key.get_secret_value() if hasattr(key, "get_secret_value")
            else str(key) if key else ""
        )

    async def research(self, company: str, role: str, location: str = "") -> str:
        if not _HAS_GENAI:
            return "google-genai SDK not installed; google agent disabled."
        if not self.api_key:
            return "Google agent error: missing API key."

        loc = f" in {location}" if location else ""
        prompt = f"""\
You are researching '{company}'{loc} for someone applying to a '{role}' role.
Use Google Search to gather CURRENT information across THREE areas. For each,
return concrete details with source URLs you actually consulted.

1. Interview process for {role}: typical stages, format, specific reported
   questions, difficulty level. Look at Glassdoor, Blind, Reddit, LinkedIn,
   LeetCode discussions, candidate blog posts.

2. Compensation for {role}: salary / total-comp ranges from Levels.fyi,
   Glassdoor, Blind, Comparably. Break down base / equity / bonus if reported.

3. Company culture & work environment (recent, 2024-2026): management style,
   work-life balance, remote/hybrid policy, recent news, strengths and
   red flags from employee reviews.

Return a SINGLE JSON object (no markdown fences) with this exact shape:
{{
  "interview":    {{"summary": "...", "sources": ["url1", "url2"]}},
  "compensation": {{"summary": "...", "sources": ["url1", "url2"]}},
  "culture":      {{"summary": "...", "sources": ["url1", "url2"]}}
}}"""

        client = genai.Client(api_key=self.api_key)

        try:
            response = await client.aio.models.generate_content(
                model="gemini-2.5-flash",
                contents=prompt,
                config=genai_types.GenerateContentConfig(
                    tools=[genai_types.Tool(
                        google_search=genai_types.GoogleSearch()
                    )],
                ),
            )

            text = _strip_fences(_response_text(response))
            grounding_urls = _extract_grounding_urls(response)

            try:
                parsed = json.loads(text)
                if grounding_urls:
                    parsed["_grounding_sources"] = grounding_urls
                return json.dumps(parsed, indent=2)
            except json.JSONDecodeError:
                return json.dumps(
                    {"raw": text, "_grounding_sources": grounding_urls},
                    indent=2,
                )

        except Exception as exc:
            # Fallback: same prompt without search grounding (LLM training data
            # only — less current, but better than nothing).
            try:
                response = await client.aio.models.generate_content(
                    model="gemini-2.5-flash",
                    contents=prompt,
                )
                text = _strip_fences(_response_text(response))
                try:
                    data = json.loads(text)
                except json.JSONDecodeError:
                    data = {"raw": text}
                return json.dumps(
                    {
                        "warning": "Google Search grounding unavailable — "
                                   "using LLM training-data only (not real-time).",
                        "grounding_error": str(exc),
                        "data": data,
                    },
                    indent=2,
                )
            except Exception as fallback_exc:
                return f"Google agent error: {exc} (fallback failed: {fallback_exc})"


def _response_text(response) -> str:
    """Extract text from a google-genai response across SDK variations."""
    if hasattr(response, "text") and response.text:
        return response.text
    try:
        parts = response.candidates[0].content.parts
        return "".join(getattr(p, "text", "") for p in parts)
    except Exception:
        return str(response)


def _extract_grounding_urls(response) -> list[str]:
    """Pull cited URLs out of Gemini's grounding metadata, if present."""
    urls: list[str] = []
    try:
        for cand in getattr(response, "candidates", None) or []:
            gm = getattr(cand, "grounding_metadata", None)
            if not gm:
                continue
            for chunk in getattr(gm, "grounding_chunks", None) or []:
                web = getattr(chunk, "web", None)
                if web and getattr(web, "uri", None):
                    urls.append(web.uri)
    except Exception:
        pass
    # De-duplicate, preserve order
    seen: set[str] = set()
    return [u for u in urls if not (u in seen or seen.add(u))]
