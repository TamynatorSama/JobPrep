# InterPrep Autofill — browser extension

Auto-fills job application forms from the resume + job data already in the
InterPrep desktop app, and asks you whenever it hits a question it can't answer
from your resume. Answers you type are remembered and reused on the next form.

## How it works

```
popup ──┐                          ┌── chrome.scripting (allFrames): scrape / fill
        ├── single page            │   / attach resume / find+click Next
background.js (SW) ── multi-page ──┘   (functions in inject.js)
        │
        └─fetch (X-InterPrep-Token)──▶ http://127.0.0.1:<port>  (InterPrep sidecar)
              /store/jobs  /store/resumes  /store/resume-file/<job_id>
              /autofill/answer-fields  /autofill/remember
```

Scraping + filling is injected into **all frames** (function injection via
`chrome.scripting`), so application forms that sit in a cross-origin iframe
(iCIMS, embedded Greenhouse/Lever) work. That needs the `<all_urls>` host
permission — the extension only acts on the tab when you click it (activeTab).
Password fields are never scraped or filled.

The extension talks **directly to the local InterPrep backend** — the same
Python sidecar the desktop app runs. So **InterPrep must be open** for the
extension to work (the backend only lives while the app is running).

## Install (unpacked, dev)

**Chrome / Edge:**

1. Open `chrome://extensions` (or `edge://extensions`).
2. Turn on **Developer mode**.
3. **Load unpacked** → select this `extension/` folder.

**Firefox (121+):**

1. Open `about:debugging#/runtime/this-firefox`.
2. **Load Temporary Add-on…** → pick any file inside this `extension/` folder
   (e.g. `manifest.json`).

The same folder loads in both: the manifest declares a service worker for Chrome
and a background event page for Firefox (`background.scripts`), and the JS uses
the promise-based `browser.*` namespace (which `chrome.*` aliases to on Chrome
MV3). A temporary add-on is removed when Firefox restarts; reload it from
`about:debugging`. (Firefox <121 isn't supported — see `strict_min_version`.)

## Pair with InterPrep

The backend protects its bridge endpoints with a per-session token, so a random
web page can't read your resume or spend your Gemini quota.

1. Launch the InterPrep desktop app (start the backend).
2. Open `%LOCALAPPDATA%\InterPrep\bridge.json` — it contains the live `port` and
   `token`. Build the **pairing code** = base64 of `"<port>:<token>"`.
   - PowerShell one-liner:
     ```powershell
    $b = Get-Content "$env:LOCALAPPDATA\InterPrep\bridge.json" | ConvertFrom-Json
    [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes("$($b.port):$($b.token)"))
     ```
3. Click the extension icon → paste the pairing code → **Pair**.

> The pairing code changes each time the app restarts (the port is dynamic and
> the token is regenerated). Re-pair if "InterPrep not reachable" shows up.

## Capture a job from a JD page

On a job posting, click the extension → **Create new job from this page**. It
first tries fast heuristics (schema.org JobPosting JSON-LD / page DOM) to fill
the detected-job card — company / role / location, plus chips for employment
type, work mode, posted date and salary when the page exposes them. If the page
is unclear it **sends the page text to Gemini**, which also pulls out the **key
requirement** chips (shown with an "AI parsed" badge). Hit **Re-analyze with AI**
to force the LLM pass, or expand **Edit details** to correct a field. Then
**Capture job description**. The desktop app (must be open) picks it up within a
few seconds, creates the job, and starts company research you can watch stream
in the app.

> One capture is processed at a time, and not while another InterPrep stream is
> running — the research will start as soon as the app is free.

## Use (autofill)

1. Go to a job application page.
2. Click the extension → on **Which job is this for?** tap the job card (its
   tailored resume + company research are used automatically), or **No specific
   job** to fill from your master resume only.
3. The **scan preview** shows what will be filled: every mapped field with its
   value, an **X of Y ready** count, and a **Single page / Multi-page** badge.
   Fields it can't answer confidently are marked *Skipped — needs you*.
4. **Single-page** forms → **Autofill N fields** fills the page (+ attaches your
   résumé PDF). Anything low-confidence shows under **Review & confirm**; confirm
   it, then you land on the **summary**.
5. **Multi-page** forms → the **stepper** view. Either **Fill step & continue**
   (fills one step, pauses for you to advance) or **Autofill all steps** (drives
   the whole flow, pausing only on fields that need you — see Safety below).
6. The **summary** screen reports fields filled, pages, and est. time saved,
   lists any fields **left blank for you**, and offers **Jump to first blank
   field** and **Log to &lt;company&gt; timeline**. Answers you confirm along the
   way are remembered and auto-fill the same questions next time.

> **Timeline logging is queued, not yet applied.** "Log to … timeline" POSTs an
> event to the backend's `timeline.json`; recording it on the job needs a small
> poller in the desktop app (mirrors the existing JD-capture inbox) — that piece
> is a follow-up.

## Safety (multi-page auto-fill)

The auto-run is deliberately conservative — it drives the form but never submits:

- **Never clicks Submit / Apply / Finish.** It stops at the submit step so you
  review and send the application yourself.
- **Pauses on anything it can't confidently answer** — it won't advance past a
  page with unanswered or low-confidence fields.
- Caps at 12 pages, stops if a page doesn't change after clicking Next (likely a
  validation error), and you can hit **Stop** any time.
- The loop runs in the extension's background worker, so closing the popup
  doesn't stop it. Reopen the popup any time — it restores the progress log and
  the pending "Review & confirm" prompt so you can finish answering and continue.

## Security notes

- Every bridge request carries the `X-InterPrep-Token` shared secret. Requests
  without it get `401`.
- The Gemini API key is held **in memory** by the backend (seeded by the Rust
  shell) and is **never** sent to the extension or written to disk.
- The backend binds to `127.0.0.1` only.
- If you change your Gemini key in InterPrep Settings, the app reseeds the
  backend automatically; no re-pairing needed.

## Known limits (MVP)

- Standard `<form>` inputs, `<select>`, radio groups, checkboxes, textareas —
  in any frame (incl. cross-origin iframes like iCIMS).
- Custom JS combobox widgets and shadow-DOM components aren't scraped yet.
- Fields that load after you scan won't be seen — re-scan after the form expands.
- The multi-page **stepper labels** are best-effort (Workday + common step
  patterns). When no recognizable stepper is found it falls back to a "Page N"
  counter; the fill itself works regardless.
- **Time saved** on the summary is an estimate (≈12s/field + 20s/page), and the
  **profile name** comes from the selected job's tailored résumé (master-resume-
  only fills show "Your profile").
