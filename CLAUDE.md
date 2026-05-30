# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Platform

Windows-only. The bundled audio recorder uses WASAPI (the `wasapi` crate), credentials live in Windows Credential Manager via `keyring`, and the sidecar uses `taskkill /T /F` to clean up Playwright's Chromium tree. Develop and test from PowerShell on Windows.

## Common commands

```powershell
# Frontend (React + Vite, runs at http://localhost:1420)
npm install
npm run dev                 # Vite dev server only — no Tauri runtime
npm run build               # tsc --noEmit + vite build into ./dist

# Full app (frontend + Rust shell + Python sidecar)
npm run tauri:dev           # Tauri auto-starts `npm run dev` per tauri.conf.json
npm run tauri:build         # production bundle

# Python backend (run once)
cd backend
.\setup.ps1                 # creates .venv (Python 3.11/3.12), installs deps + `playwright install chromium`

# Rust (from src-tauri/)
cargo check                 # fast type-check — preferred for quick iteration
cargo clippy
cargo fmt
```

No test suite exists (`cargo test` is a no-op; no `pytest` config; no Vitest). `backend/test_research.py` is an ad-hoc script, not pytest.

`src-tauri/src/main.rs` is a 7-line shim — there is no CLI surface left. The headless `--seconds` / `--list-devices` flags described in `README.md` and `AGENTS.md` no longer exist; the recorder is reachable only through the Tauri commands `list_audio_devices` / `start_recording` / `stop_recording`. Update README before believing it.

## Architecture

Three-tier desktop app: React UI → Tauri Rust shell (IPC + native services) → Python FastAPI sidecar (LLM + scraping). The crate name is `interprep`; the product is InterPrep, an AI interview-prep coach.

### Frontend — `src/` (React + TypeScript, single file)

`src/App.tsx` (~3.1k lines) holds the entire UI in one component tree with local React state. `src/styles.css` and the `T = { ... }` token block at the top of `App.tsx` carry the dark theme. Resume uploads parse PDF (`pdfjs-dist` worker URL imported via Vite's `?url` suffix), DOCX (`mammoth`), MD/TXT in-process before sending text to the backend.

All backend access goes through `invoke(...)` and `listen(...)` from `@tauri-apps/api` — there is no `fetch`/HTTP layer in the frontend.

### Rust shell — `src-tauri/src/`

`main.rs` → `lib.rs::run()` registers state and IPC handlers. Modules:

- **`sidecar.rs`** — spawns the Python FastAPI process (`uvicorn main:app`). Discovery order: `INTERPREP_BACKEND_DIR` env → walk up to 6 levels from `current_exe()` → walk up to 6 levels from `current_dir()` looking for `backend/main.py`. Python order: `backend/.venv/Scripts/python.exe` → bundled `python/python.exe` → `py -3.12` / `py -3.11` / `python3.x`. Polls `/health` for up to 30s; on `ExitRequested` runs `taskkill /T /F /PID <pid>` plus `Child::kill` to wipe the whole tree (uvicorn worker + Playwright `chrome.exe` + renderer subprocesses).
- **`backend_client.rs`** — `reqwest::blocking` POSTs to `/{chat|research|company-research|application}` endpoints and forwards each SSE `data:` line to the frontend as a window event. SSE event types map to Tauri events: `token`→`chat:token`, `stage`→`chat:log`, `scorecard`→`chat:scorecard`, `resume_docx`→`chat:resume_docx`, `tailored_resume`→`chat:tailored_resume`, `done`→`chat:done`, `error`→`chat:error`. The frontend uses the same event channel for every workflow — there's no per-stream namespacing, so only one stream should run at a time.
- **`credentials.rs`** — Windows Credential Manager (`keyring` crate, service `"InterPrep"`). One entry per field. Empty value = delete entry.
- **`jobs_store.rs`** / **`resume_store.rs`** — JSON files under `%LOCALAPPDATA%\InterPrep\` (`jobs.json`, `resumes.json`). Atomic write via temp-file + rename so a mid-write crash never corrupts the file.
- **`types.rs`** — `Job` / `ChatThread` / `ChatMsg` / `StageNote`. `serde(rename_all = "camelCase")` on `Job` for JS interop; IDs are `String` because the frontend mints them with `Date.now()`.
- **`recorder.rs`** — unchanged WASAPI dual-stream backend (system audio via render-loopback + mic via capture, both written as 32-bit float WAV via `hound`). Each worker initializes COM (MTA) on its own thread via `ComGuard`; an `Arc<AtomicBool>` stop flag is shared so one worker's error trips the other. `save_resume_docx` decodes the base64 `.docx` from `chat:resume_docx` into `%APPDATA%\InterPrep\applications\<job_id>\resume.docx`.

### Python sidecar — `backend/` (FastAPI + LangGraph + Playwright)

`main.py` mounts four routers under `/chat`, `/research`, `/company-research`, `/application`. The lifespan handler kicks `agents.company_research.browser_manager.get_browser()` as a background task so `/health` answers fast (the Rust side gives up after 30s). Models in `models.py` (Pydantic).

Routes:

- **`routes/chat.py`** — coach mode (`gemini-2.5-flash`) and live `interviewer` mode (`gemini-2.5-pro` for persona consistency). Streams via `ChatGoogleGenerativeAI(streaming=True).astream(...)`.
- **`routes/research.py`** — JD-only role-fit analysis. LangGraph workflow defined in `agents/workflow.py` with nodes `extract → questions → tips`. Stage banners emitted on `on_chain_start`; tokens forwarded from `on_chat_model_stream`.
- **`routes/company_research.py` + `agents/company_research/`** — supervisor/agent graph. `orchestrator.py` builds `supervisor → (glassdoor | indeed | google | comparably | levels | repvue)* → compose → END`. The supervisor LLM picks the next source given which are completed; site agents drive Playwright via `LLMBrowserAgent` (Glassdoor/Indeed need user credentials to bypass paywalls). `compose` is the only streaming node. The route maps LangGraph node names to user-facing stage banners via `_STAGE_BANNERS`.
- **`routes/application.py`** — `gemini-2.5-pro` resume tailor: picks the closest master resume, builds an ATS-tailored `.docx` (via `python-docx`), streams the cover letter as tokens, and emits a scorecard JSON event. Also serves `/application/knockout-screen` for recruiter-style screen simulation.

### Persistence layout

- `%LOCALAPPDATA%\InterPrep\jobs.json` — job records
- `%LOCALAPPDATA%\InterPrep\resumes.json` — master resume library
- `%LOCALAPPDATA%\InterPrep\sidecar.log` — uvicorn stdout/stderr (single source of truth for sidecar boot failures)
- `%APPDATA%\InterPrep\applications\<job_id>\resume.docx` — generated tailored resumes
- Windows Credential Manager under service `"InterPrep"` — Gemini API key, Glassdoor/Indeed logins

## Things that will trip you up

- **Don't add `fetch` calls from the frontend.** All backend traffic is `invoke` → Rust → `reqwest` to the sidecar. Adding direct HTTP from React breaks the SSE-to-Tauri-event bridge and the sidecar shutdown contract.
- **Sidecar discovery walks `current_exe()` first, then `current_dir()`.** In `tauri dev`, cargo runs from `src-tauri/`, so `./backend` doesn't exist — the walk-up from the cwd is what finds it. Don't simplify either branch without checking both `cargo run` and `npm run tauri:dev`.
- **Killing the sidecar requires `taskkill /T /F`, not `Child::kill`.** Python spawns Playwright's `chrome.exe`, which spawns renderer/GPU subprocesses. Plain `kill()` leaves orphan `chrome.exe` after window close. The `ExitRequested` handler in `lib.rs` runs before Tauri tears down — that's the only reliable window to clean up.
- **SSE events use one global channel (`chat:*`).** Two concurrent streams will interleave tokens into the same listener. The UI must single-flight streams; don't kick off a second one before `chat:done` fires.
- **COM (MTA) must be initialized per-thread before any WASAPI call.** `query_device_catalog` deliberately runs on a fresh thread so its COM init doesn't poison the caller. Preserve that pattern.
- **Recorder writes 32-bit float WAV regardless of native device format.** `write_packet_as_f32` and `make_wav_spec` in `recorder.rs` are tightly coupled — change the spec, change the writer.
- **Python 3.13/3.14 are not supported.** `playwright` and some LangChain wheels lag. `backend/setup.ps1` enforces 3.11/3.12; the Rust `find_python` fallback list matches.
- **CORS is wide open (`allow_origins=["*"]`) in `backend/main.py`.** Fine because the sidecar binds to `127.0.0.1` only and is launched as a child of the Tauri shell. Don't expose the port externally.
- **`README.md` and `AGENTS.md` are stale.** They describe a pre-Tauri, egui-based recorder with a CLI. Treat this file as the source of truth; update the others if you touch anything they describe.
