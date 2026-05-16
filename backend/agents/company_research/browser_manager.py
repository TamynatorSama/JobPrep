"""
Global Playwright browser singleton.
The browser is started once (on first use) and reused across all requests.
Sessions are saved to disk so login is only needed once per service/account pair.
"""
import os
import hashlib
from playwright.async_api import async_playwright, Browser, Playwright

_playwright: Playwright | None = None
_browser: Browser | None = None

SESSIONS_DIR = os.path.join(os.path.dirname(__file__), "..", "..", "sessions")

# Injected before any page script runs — hides the telltales most anti-bot
# fingerprinters check (navigator.webdriver, missing chrome global, empty
# plugins/languages). Doesn't beat PerimeterX or Cloudflare Turnstile, but
# helps with lighter detectors.
_STEALTH_INIT = """
Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
Object.defineProperty(navigator, 'plugins',   { get: () => [1, 2, 3, 4, 5] });
Object.defineProperty(navigator, 'languages', { get: () => ['en-US', 'en'] });
window.chrome = { runtime: {} };
const origQuery = window.navigator.permissions && window.navigator.permissions.query;
if (origQuery) {
    window.navigator.permissions.query = (p) => (
        p.name === 'notifications'
            ? Promise.resolve({ state: Notification.permission })
            : origQuery(p)
    );
}
"""


async def get_browser() -> Browser:
    global _playwright, _browser
    if _browser is None or not _browser.is_connected():
        _playwright = await async_playwright().start()
        _browser = await _playwright.chromium.launch(
            headless=True,
            args=[
                "--no-sandbox",
                "--disable-blink-features=AutomationControlled",
                "--disable-features=IsolateOrigins,site-per-process",
                "--disable-site-isolation-trials",
            ],
        )
    return _browser


async def new_context(storage_state_path: str | None = None):
    browser = await get_browser()
    kwargs: dict = {
        "viewport": {"width": 1366, "height": 768},
        "user_agent": (
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
            "AppleWebKit/537.36 (KHTML, like Gecko) "
            "Chrome/131.0.0.0 Safari/537.36"
        ),
        "locale": "en-US",
        "extra_http_headers": {
            "Accept-Language": "en-US,en;q=0.9",
            "Accept": (
                "text/html,application/xhtml+xml,application/xml;q=0.9,"
                "image/avif,image/webp,*/*;q=0.8"
            ),
            "Sec-Ch-Ua": '"Chromium";v="131", "Not_A Brand";v="24"',
            "Sec-Ch-Ua-Platform": '"Windows"',
        },
    }
    if storage_state_path and os.path.exists(storage_state_path):
        kwargs["storage_state"] = storage_state_path

    ctx = await browser.new_context(**kwargs)
    await ctx.add_init_script(_STEALTH_INIT)
    return ctx


async def shutdown():
    global _playwright, _browser
    if _browser:
        await _browser.close()
        _browser = None
    if _playwright:
        await _playwright.stop()
        _playwright = None


def session_path(service: str, email: str) -> str:
    os.makedirs(SESSIONS_DIR, exist_ok=True)
    key = hashlib.md5(f"{service}:{email}".encode()).hexdigest()[:10]
    return os.path.join(SESSIONS_DIR, f"{service}_{key}.json")
