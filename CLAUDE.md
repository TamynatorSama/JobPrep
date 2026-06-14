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
- **`credentials.rs`** — Windows Credential Manager (`keyring` crate, service `"InterPrep"`). One entry per field. Empty value = delete entry. Fields: `llm_provider` (Settings toggle: gemini | openai | anthropic) + per-provider API keys. `Credentials::llm_json()` builds the snake_case `llm` payload attached to every backend request (mirrors Python's `LLMConfig`). Legacy Glassdoor/Indeed + retired Groq/Ollama entries are deleted on save.
- **`jobs_store.rs`** / **`resume_store.rs`** — JSON files under `%LOCALAPPDATA%\InterPrep\` (`jobs.json`, `resumes.json`). Atomic write via temp-file + rename so a mid-write crash never corrupts the file.
- **`types.rs`** — `Job` / `ChatThread` / `ChatMsg` / `StageNote`. `serde(rename_all = "camelCase")` on `Job` for JS interop; IDs are `String` because the frontend mints them with `Date.now()`.
- **`recorder.rs`** — unchanged WASAPI dual-stream backend (system audio via render-loopback + mic via capture, both written as 32-bit float WAV via `hound`). Each worker initializes COM (MTA) on its own thread via `ComGuard`; an `Arc<AtomicBool>` stop flag is shared so one worker's error trips the other. `save_resume_docx` decodes the base64 `.docx` from `chat:resume_docx` into `%APPDATA%\InterPrep\applications\<job_id>\resume.docx`.

### Python sidecar — `backend/` (FastAPI + LangGraph)

`main.py` mounts routers under `/chat`, `/research`, `/company-research`, `/application`, `/voice`, plus the token-guarded browser-extension bridge (`/config`, `/store`, `/autofill`, `/inbox`). Models in `models.py` (Pydantic).

**Multi-LLM:** every request carries an `llm: LLMConfig` payload (provider toggle + per-provider keys, built by Rust from Credential Manager). `backend/llm_provider.py` is the factory all routes go through — `make_chat_model` (lazy-imports the LangChain partner package per provider), `candidate_models(cfg, tier)` ("fast"/"smart" tiers with preview→stable fallback per provider), `generate_json`/`generate_raw` (one-shot + lenient parse), `content_text` (flattens block-list content from Anthropic), `make_embeddings` (Gemini/OpenAI, with fallback — Anthropic has no embeddings API). **The module is named `llm_provider`, not `llm`, because the embedded `research_scraper` package ships its own top-level `llm` package — naming it `llm` shadows the engine's import and breaks company research.** Extension-bridge routes read the config from `runtime_config` (seeded via `POST /config/seed` on startup + Settings save).

Routes:

- **`routes/chat.py`** — coach mode (fast tier) and live `interviewer` mode (smart tier for persona consistency). Streams via the factory model's `.astream(...)`; model fallback only happens before the first token.
- **`routes/research.py`** — JD-only role-fit analysis. LangGraph workflow defined in `agents/workflow.py` with nodes `extract → questions → tips`. Stage banners emitted on `on_chain_start`; tokens forwarded from `on_chat_model_stream`.
- **`routes/company_research.py`** — backed by the embedded `research_scraper` engine (editable-installed from `D:\code\ML\research_scraper`; LangGraph plan → search → scrape → reflect → peer → audit → synthesize, HTTP/JSON scraping — no browser, no logins). The engine's LLM router speaks the OpenAI-compatible wire format with a built-in fallback chain; the route maps the selected provider (gemini | openai | anthropic) onto the custom lane via the engine's `runtime.overrides` contextvar (Anthropic rides its OpenAI-compat endpoint `https://api.anthropic.com/v1`) and passes the spare gemini key as a fallback lane. The Gemini key also powers the Google Search grounding source regardless of provider.
- **`routes/application.py`** — smart-tier resume tailor: aggregates evidence across master resumes, builds an ATS-tailored `.docx` (via `python-docx`), streams the cover letter as tokens, and emits a scorecard JSON event. Also serves `/application/knockout-screen` for recruiter-style screen simulation.

### Persistence layout

- `%LOCALAPPDATA%\InterPrep\jobs.json` — job records
- `%LOCALAPPDATA%\InterPrep\resumes.json` — master resume library
- `%LOCALAPPDATA%\InterPrep\sidecar.log` — uvicorn stdout/stderr (single source of truth for sidecar boot failures)
- `%APPDATA%\InterPrep\applications\<job_id>\resume.docx` — generated tailored resumes
- Windows Credential Manager under service `"InterPrep"` — LLM provider toggle + Gemini/OpenAI/Anthropic API keys

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
