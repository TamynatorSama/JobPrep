# InterPrep

**Open-source AI interview prep coach for Windows.**

InterPrep helps you track job applications, research roles and companies, practice with a coach or live mock interviewer, and generate tailored application materials — all from a local desktop app. Your data stays on your machine; you bring your own API keys.

## Features

- **Job pipeline** — Track applications through stages (Applied → Screen → Technical → Offer) with a timeline view.
- **Role-fit research** — Paste a job description and get structured analysis: explicit requirements, implicit signals, and prep tips.
- **Company research** — Multi-source agent workflow (plan → search → scrape → reflect → audit → synthesize) over public HTTP/JSON sources. No browser, no site logins.
- **Coach chat** — Ask follow-up questions about a role with context from your job record and uploaded documents.
- **Mock interviews** — Live interviewer mode with configurable focus areas, difficulty, and question style. Optional voice mode with spoken Q&A and barge-in.
- **Interview copilot** — A movable, screen-capture-cloaked overlay that listens to the call, transcribes each interviewer question, and drafts an answer live, alongside a per-job cheatsheet.
- **Application prep** — ATS-tailored resume (`.docx`), cover letter streaming, and a knockout-screen simulation.
- **Browser extension** — Auto-fills job applications and creates job records from postings via a token-guarded local bridge.
- **Resume library** — Upload and manage master resumes (PDF, DOCX, MD, TXT) used across workflows.
- **Multi-LLM** — Bring your own Google Gemini, OpenAI, or Anthropic API key and switch provider in Settings.
- **Local-first storage** — Jobs, resumes, and chat history persist under `%LOCALAPPDATA%\InterPrep\`. API keys live in Windows Credential Manager.

## Architecture

InterPrep is a three-tier desktop app:

```
React UI (Vite + TypeScript)
        ↕  Tauri IPC
Rust shell (sidecar lifecycle, credentials, audio, file I/O)
        ↕  HTTP / SSE on 127.0.0.1
Python sidecar (FastAPI + LangGraph)
```

The frontend never calls the backend directly — all traffic goes through Tauri `invoke` handlers, which stream SSE events back as window events (`chat:token`, `chat:done`, etc.).

## Requirements

| Tool | Version |
|------|---------|
| Windows | 10 or 11 (64-bit) |
| [Rust](https://rustup.rs/) | stable |
| [Node.js](https://nodejs.org/) | 18+ |
| Python | **3.11 or 3.12 only** (3.13+ lacks wheels for Playwright / some ML deps) |

Optional for voice mode: NVIDIA GPU + CUDA (faster TTS); CPU fallback is supported but slower.

## Getting started

### 1. Clone the repository

```powershell
git clone https://github.com/YOUR_ORG/interprep.git
cd interprep
```

Replace the URL with your fork or the canonical repo once published.

### 2. Set up the Python backend (once)

```powershell
cd backend
.\setup.ps1
```

This creates `backend/.venv`, installs dependencies, and installs the company-research engine from a sibling `research_scraper` checkout when present.

For optional voice mode (local STT/TTS):

```powershell
.\setup.ps1 -Voice
```

### 3. Install frontend dependencies

```powershell
cd ..
npm install
```

### 4. Run in development

```powershell
npm run tauri:dev
```

Tauri starts the Vite dev server and launches the desktop window. The Rust shell auto-spawns the Python sidecar and polls `/health` before marking the backend ready.

### 5. Configure API keys

Open **Settings → API Keys** in the app, pick a provider (Google Gemini, OpenAI, or Anthropic), and paste that provider's API key — e.g. a [Gemini key](https://aistudio.google.com/apikey). A Gemini key is also used for search grounding during company research, whichever provider you select.

## Common commands

```powershell
# Frontend only (no Tauri shell)
npm run dev

# Type-check + production frontend build
npm run build

# Full desktop app — development
npm run tauri:dev

# Production installer / bundle
npm run tauri:build

# Rust type-check (from src-tauri/)
cargo check
```

## Project layout

```
src/                  React UI (App.tsx, styles)
src-tauri/src/        Rust shell — IPC, sidecar, credentials, WASAPI recorder
backend/              Python FastAPI sidecar — LLM agents, scraping, voice
backend/agents/       LangGraph workflows (research, company research)
```

### Local data paths

| Path | Contents |
|------|----------|
| `%LOCALAPPDATA%\InterPrep\jobs.json` | Job records |
| `%LOCALAPPDATA%\InterPrep\resumes.json` | Master resume library |
| `%LOCALAPPDATA%\InterPrep\sidecar.log` | Sidecar stdout/stderr |
| `%APPDATA%\InterPrep\applications\<job_id>\resume.docx` | Generated tailored resumes |
| Windows Credential Manager (`InterPrep` service) | API keys, site logins |

## Privacy & security

- The Python sidecar binds to **127.0.0.1** only and is launched as a child of the Tauri process — it is not exposed to the network.
- Credentials never touch disk as plain text; they are stored in Windows Credential Manager.
- LLM calls go to the provider you select (Google Gemini, OpenAI, or Anthropic) using **your** API key. Review your provider's terms before use.
- Company research scrapes only public pages over HTTP — no browser automation, no site logins.

## Contributing

Contributions are welcome. This project is early-stage; issues and pull requests help shape direction.

1. Fork the repo and create a branch from `main`.
2. Make your changes. Match existing style — minimal diffs, no unrelated refactors.
3. Test locally with `npm run tauri:dev` and `cargo check`.
4. Open a pull request with a clear description of what changed and why.

There is no automated test suite yet. Manual verification of the workflow you touched is appreciated.

## Roadmap / known gaps

- Windows-only today (WASAPI audio, Credential Manager, sidecar lifecycle).
- `AGENTS.md` and parts of older docs may still reference the pre-Tauri prototype — treat this README and `CLAUDE.md` as current.
- Voice mode requires the optional `-Voice` backend install.

## License

InterPrep is open source software released under the [MIT License](LICENSE).

## Acknowledgments

Built with [Tauri](https://tauri.app/), [React](https://react.dev/), [FastAPI](https://fastapi.tiangolo.com/), [LangGraph](https://www.langchain.com/langgraph), [Playwright](https://playwright.dev/), and [Google Gemini](https://ai.google.dev/).
