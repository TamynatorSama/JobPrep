# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Platform

Windows-only. The audio backend depends on WASAPI (the `wasapi` crate) and would not compile or function on macOS/Linux. Develop and test from PowerShell on Windows.

## Common commands

```powershell
cargo build                 # debug build
cargo run                   # launch the InterPrep desktop UI
cargo run -- --gui          # same, explicit flag
cargo run -- --list-devices # enumerate active render and capture devices, then exit
cargo run -- --seconds 30   # headless: record both streams for 30s
cargo run -- --seconds 0    # headless: record until Ctrl+C
cargo check                 # fast type-check; preferred over build for quick iteration
cargo clippy                # lint
cargo fmt                   # format
```

No test suite exists yet — `cargo test` is a no-op.

Headless mode flags: `--system-device`, `--mic-device` (friendly names from `--list-devices`), `--out-dir`, `--system-file`, `--mic-file`, `--poll-timeout-ms`. See `Args` in `src/main.rs`.

## Architecture

Two modules with sharply different concerns:

- **`src/recorder.rs`** — WASAPI capture backend. Independent of the UI.
  - `RecordingSession::start` spawns one capture worker thread per source (system audio via render-loopback, microphone via capture). Each worker initializes COM (MTA) on its own thread (`ComGuard` calls `wasapi::deinitialize` on drop), resolves a `Device`, opens an event-driven shared-mode stream, and pumps packets into a `hound::WavWriter`.
  - All samples are decoded to `f32` and written as 32-bit float WAV regardless of native format (`write_packet_as_f32` / `decode_sample_to_f32` handle 8/16/24/32-bit PCM and 32-bit float).
  - Coordination: a single `Arc<AtomicBool>` stop flag shared by the session and both workers; if either errors it sets the flag so the other stops too.
  - Public API: `RecorderConfig`, `RecordingSession`, `RecordingReport`, `query_device_catalog`, `print_device_catalog`, `print_capture_summary`, `DEFAULT_POLL_TIMEOUT_MS`.

- **`src/gui/`** — `eframe`/`egui` desktop shell split into submodules:
  - `app.rs` — `InterPrepApp` struct holds all UI state; `eframe::App` impl; methods for `send_message`, `create_job`, `start_research_for_job`, `start_typed_thread`.
  - `types.rs` — `Job`, `ChatThread`, `ChatMsg`, `StageNote`, `JobStatus`, `Screen`, `MsgRole`.
  - `theme.rs` — colour palette constants (`BG`, `SURFACE`, `INK`, `MAGENTA`, `BD`, etc.) and `apply_theme`.
  - `utils.rs` — `render_markdown`, day/date helpers, `sample_jobs` seed data.
  - `views/sidebar.rs` — left icon rail + collapsible job list sidebar.
  - `views/chat.rs` — chat workspace (header, message stream, composer).
  - `views/timeline.rs` — Gantt-style application timeline.
  - `views/modals.rs` — "Add New Job" modal + Settings modal (including Gemini API key field).

  **All UI state is mock data** seeded by `sample_jobs()`. Nothing persists across launches. The AI backend is being rethought — `send_message`, `start_research_for_job`, and `start_typed_thread` currently return placeholder messages.

## Things that will trip you up

- The recorder backend and GUI **do not talk to each other**. There is no "start recording" button in the UI. Wiring them requires running the WASAPI session off the UI thread and bridging state back via a channel.
- COM must be initialized per thread (`init_mta`) before any WASAPI call. Don't move device enumeration or capture work onto a thread that hasn't done this.
- `query_device_catalog` deliberately runs on a freshly-spawned thread so its COM init does not poison the caller. Preserve that pattern for any similar queries.
- Output is always 32-bit float WAV. If you change `make_wav_spec`, also update `write_packet_as_f32` — they are tightly coupled.
- `UiSeed::from_args` ignores all CLI args — the GUI does not yet honor `--out-dir`, `--system-file`, etc.
