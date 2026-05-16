"""
LLMBrowserAgent — a Playwright page controlled by an LLM.

The agent loops:
  1. Capture a text snapshot of the current page.
  2. Ask the LLM what to do next (JSON action).
  3. Execute the action via Playwright.
  4. Repeat until the LLM returns {"action": "done", "data": {...}} or max steps.

Diagnostics (opt-in, no-op for the real app):
  BROWSER_AGENT_VERBOSE=1            prints step-by-step progress
  BROWSER_AGENT_TRACE_DIR=<path>     writes snapshot + action + result files
"""
import asyncio
import json
import os
import re
from pathlib import Path

from playwright.async_api import Page
from langchain_core.messages import HumanMessage
from langchain_google_genai import ChatGoogleGenerativeAI

ACTION_SCHEMA = """\
Respond with a single JSON object (no markdown fences):
  Click element:    {"action": "click",    "text": "<visible label/text of element>"}
  Fill input:       {"action": "fill",     "label": "<placeholder or label>", "value": "<text>"}
  Press key:        {"action": "press",    "key": "Enter"}
  Navigate to URL:  {"action": "goto",     "url": "https://..."}
  Scroll down:      {"action": "scroll"}
  Extraction done:  {"action": "done",     "data": {<extracted JSON data>}}
  Cannot proceed:   {"action": "failed",   "reason": "<why>"}"""


# ── Anti-bot detection ──────────────────────────────────────────────────────
# Most site-specific scrapers hit a CDN-level bot wall (PerimeterX, Cloudflare,
# DataDome, Akamai). Call this right after page.goto() so we can skip the
# expensive LLM loop instead of letting it flail on a challenge page.

_BLOCK_TITLE_MARKERS = (
    "access denied",
    "access to this page",
    "blocked",
    "captcha",
    "robot check",
    "are you human",
    "verify you are human",
    "cloudflare",
    "attention required",
    "just a moment",
    "security check",
    "about this page",          # google /sorry/ pages
)

_BLOCK_BODY_MARKERS = (
    "press & hold",
    "press and hold",
    "verify you are a human",
    "checking your browser",
    "review the security of your connection",
    "please complete the security check",
    "enable javascript and cookies to continue",
    "captcha",
    "perimeter-x",
    "perimeterx",
    "unusual traffic",                            # google
    "not a robot",                                # google /sorry/
    "this page checks to see if it's really you", # google /sorry/
    "automated requests",                         # google /sorry/
    "datadome",                                   # datadome
    "px-captcha",                                 # perimeterx
)

# URL substrings that are themselves a block signal
_BLOCK_URL_MARKERS = (
    "/sorry/index",          # google
    "/sorry/?continue",
    "challenge-platform",    # cloudflare
    "px-captcha",            # perimeterx
    "/_Incapsula_Resource",  # imperva
    "datadome.co",           # datadome
)


async def detect_block(page: Page) -> str | None:
    """Return a short reason string if the page is a bot challenge, else None."""
    try:
        title = (await page.title() or "").lower()
        for m in _BLOCK_TITLE_MARKERS:
            if m in title:
                return f"title: {m}"

        body = await page.evaluate(
            "() => (document.body && document.body.innerText || '').slice(0, 1500).toLowerCase()"
        )
        for m in _BLOCK_BODY_MARKERS:
            if m in body:
                return f"content: {m}"
    except Exception:
        pass
    return None


def snapshot_is_blocked(snap: str) -> str | None:
    """Same heuristics as detect_block(), but operates on the snapshot string
    we already captured — avoids a second round-trip to the page.

    Snapshot layout (see LLMBrowserAgent.snapshot()):
        URL: …
        Title: …
        Visible text:
        …
    We check the URL + title region (first ~400 chars) for URL/title markers
    and the full snapshot for body markers.
    """
    lower = snap.lower()
    head = lower[:400]  # URL + Title region
    for m in _BLOCK_URL_MARKERS:
        if m in head:
            return f"url: {m}"
    for m in _BLOCK_TITLE_MARKERS:
        if m in head:
            return f"title: {m}"
    for m in _BLOCK_BODY_MARKERS:
        if m in lower:
            return f"content: {m}"
    return None


class LLMBrowserAgent:
    # Class-level counter so each instance gets a unique trace prefix
    # within a single test run (otherwise files would overwrite).
    _trace_counter = 0

    def __init__(
        self,
        page: Page,
        llm: ChatGoogleGenerativeAI,
        max_steps: int = 14,
        verbose: bool | None = None,
        trace_dir: str | None = None,
        trace_label: str | None = None,
    ):
        self.page = page
        self.llm = llm
        self.max_steps = max_steps

        # Defaults pulled from env so the real app stays silent unless
        # diagnostics are explicitly enabled.
        if verbose is None:
            verbose = os.environ.get("BROWSER_AGENT_VERBOSE", "0") == "1"
        if trace_dir is None:
            trace_dir = os.environ.get("BROWSER_AGENT_TRACE_DIR")
        self.verbose = verbose
        self.trace_dir = Path(trace_dir) if trace_dir else None

        LLMBrowserAgent._trace_counter += 1
        self.trace_prefix = f"{LLMBrowserAgent._trace_counter:02d}"
        if trace_label:
            self.trace_prefix = f"{self.trace_prefix}_{trace_label}"

    # ── Diagnostics helpers ──────────────────────────────────────────────────

    def _vprint(self, msg: str) -> None:
        if self.verbose:
            print(msg, flush=True)

    def _save_trace(self, step: int, name: str, content) -> None:
        if not self.trace_dir:
            return
        self.trace_dir.mkdir(parents=True, exist_ok=True)
        fp = self.trace_dir / f"{self.trace_prefix}_step{step:02d}_{name}.txt"
        try:
            with fp.open("w", encoding="utf-8") as f:
                if isinstance(content, (dict, list)):
                    json.dump(content, f, indent=2, ensure_ascii=False)
                else:
                    f.write(str(content))
        except Exception:
            pass

    # ── Page introspection ────────────────────────────────────────────────────

    async def snapshot(self) -> str:
        url = self.page.url
        title = await self.page.title()

        # Cleaned page text (remove noisy tags)
        text = await self.page.evaluate("""() => {
            const clone = document.body.cloneNode(true);
            ['script','style','svg','img','video','noscript','iframe'].forEach(t =>
                clone.querySelectorAll(t).forEach(e => e.remove())
            );
            return (clone.innerText || '').replace(/\\s{2,}/g, ' ').trim().slice(0, 5000);
        }""")

        # Visible interactive elements with index (for precise targeting)
        elements = await self.page.evaluate("""() => {
            const sel = 'a[href], button, input, textarea, select, [role="button"], [role="link"], [role="menuitem"]';
            const els = [];
            document.querySelectorAll(sel).forEach((el, i) => {
                const r = el.getBoundingClientRect();
                if (r.width <= 0 || r.height <= 0) return;
                if (r.top > window.innerHeight + 200 || r.bottom < -200) return;
                const text = (el.innerText || el.value || el.placeholder ||
                              el.getAttribute('aria-label') || el.getAttribute('title') || '').trim().slice(0, 60);
                if (!text) return;
                els.push({i, tag: el.tagName.toLowerCase(), type: el.type || '', text, href: (el.href||'').slice(0,80)});
            });
            return els.slice(0, 25);
        }""")

        return (
            f"URL: {url}\n"
            f"Title: {title}\n\n"
            f"Visible text:\n{text[:3000]}\n\n"
            f"Interactive elements:\n{json.dumps(elements, indent=2)}"
        )

    # ── LLM decision ─────────────────────────────────────────────────────────

    async def decide(self, snap: str, goal: str) -> dict:
        prompt = (
            f"You are controlling a headless browser to accomplish:\n{goal}\n\n"
            f"Current page state:\n{snap}\n\n"
            f"{ACTION_SCHEMA}"
        )
        resp = await self.llm.ainvoke([HumanMessage(content=prompt)])
        raw = resp.content.strip()

        # Strip markdown fences if present
        raw = re.sub(r"^```(?:json)?\s*", "", raw, flags=re.MULTILINE)
        raw = re.sub(r"\s*```$", "", raw, flags=re.MULTILINE)

        return json.loads(raw)

    # ── Action execution ──────────────────────────────────────────────────────

    async def execute(self, action: dict) -> tuple[bool, dict]:
        """Returns (should_continue, result_data)."""
        act = action.get("action", "")

        try:
            if act == "click":
                text = action.get("text", "")
                clicked = False
                for locator in [
                    self.page.get_by_text(text, exact=True),
                    self.page.get_by_text(text, exact=False),
                    self.page.get_by_role("button", name=text),
                    self.page.get_by_role("link", name=text),
                ]:
                    try:
                        await locator.first.click(timeout=4000)
                        clicked = True
                        break
                    except Exception:
                        continue
                if clicked:
                    try:
                        await self.page.wait_for_load_state("domcontentloaded", timeout=12000)
                    except Exception:
                        pass

            elif act == "fill":
                label = action.get("label", "")
                value = action.get("value", "")
                for locator in [
                    self.page.get_by_placeholder(label, exact=False),
                    self.page.get_by_label(label, exact=False),
                ]:
                    try:
                        await locator.first.fill(value, timeout=4000)
                        break
                    except Exception:
                        continue

            elif act == "press":
                key = action.get("key", "Enter")
                await self.page.keyboard.press(key)
                try:
                    await self.page.wait_for_load_state("domcontentloaded", timeout=10000)
                except Exception:
                    pass

            elif act == "goto":
                await self.page.goto(action["url"], wait_until="domcontentloaded", timeout=30000)

            elif act == "scroll":
                await self.page.evaluate("window.scrollBy(0, window.innerHeight * 0.8)")
                await asyncio.sleep(0.6)

            elif act == "done":
                return False, action.get("data", {})

            elif act == "failed":
                return False, {}

        except Exception:
            pass  # LLM will see the same page and try a different action

        await asyncio.sleep(0.4)  # polite delay
        return True, {}

    # ── Run loop ──────────────────────────────────────────────────────────────

    async def run(self, goal: str) -> dict:
        """Execute the goal loop. Returns extracted data dict.

        Auto-aborts (without calling the LLM) when the current snapshot looks
        like a human-verification / bot challenge. The returned dict contains
        a `_blocked` key in that case so callers can detect and skip cleanly.
        """
        self._vprint(f"\n  ▶ GOAL [{self.trace_prefix}]: {goal[:140]}{'…' if len(goal) > 140 else ''}")
        self._save_trace(0, "goal", goal)

        for step in range(self.max_steps):
            snap = await self.snapshot()
            self._save_trace(step, "snapshot", snap)

            if self.verbose:
                header = snap.split("\n\n", 1)[0]  # "URL: …\nTitle: …"
                el_count = snap.count('"i":')
                print(f"  ─ step {step}: {header.replace(chr(10), ' | ')}  ({el_count} elements)", flush=True)

            # Human-verification / bot-challenge detector. Trips on any step
            # (initial page-load OR mid-run navigation) and bails immediately
            # so we don't burn LLM calls trying to defeat a CAPTCHA.
            block = snapshot_is_blocked(snap)
            if block:
                self._vprint(f"  ⚠ bot challenge detected at step {step} ({block}) — stopping")
                self._save_trace(step, "blocked", {"reason": block, "step": step})
                return {"_blocked": block, "_step": step}

            try:
                action = await self.decide(snap, goal)
            except Exception as exc:
                self._vprint(f"  ✗ LLM decision failed: {exc}")
                self._save_trace(step, "decision_error", str(exc))
                break

            self._save_trace(step, "action", action)
            if self.verbose:
                summary = json.dumps(action, ensure_ascii=False)
                print(f"    → {summary[:240]}{'…' if len(summary) > 240 else ''}", flush=True)

            should_continue, data = await self.execute(action)
            if not should_continue:
                self._save_trace(step, "result", data)
                if action.get("action") == "done":
                    keys = list(data.keys()) if isinstance(data, dict) else []
                    self._vprint(f"  ✓ done — {len(keys)} keys: {keys}")
                else:
                    self._vprint(f"  ✗ aborted ({action.get('action')}): {action.get('reason', '')}")
                return data

        self._vprint(f"  ⚠ hit max_steps ({self.max_steps}) — returning empty")
        return {}
