# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Platform

Windows-only. The audio backend depends on WASAPI (the `wasapi` crate) and would not compile or function on macOS/Linux. Develop and test from PowerShell on Windows.

## Common commands

```powershell
cargo build                 # debug build
cargo run                   # launch the JobServe desktop UI (no-args path forces --gui)
cargo run -- --gui          # same, explicit
cargo run -- --list-devices # enumerate active render and capture devices, then exit
cargo run -- --seconds 30   # headless: record both streams for 30s into ./captures/
cargo run -- --seconds 0    # headless: record until Ctrl+C
cargo check                 # fast type-check; preferred over `build` for quick iteration
cargo clippy                # lint
cargo fmt                   # format
```

There is no test suite in this repo — no `cargo test` invocations are meaningful yet.

Headless mode flags worth knowing: `--system-device`, `--mic-device` (friendly names from `--list-devices`), `--out-dir`, `--system-file`, `--mic-file`, `--poll-timeout-ms`. See `Args` in `src/main.rs`.

## Architecture

The crate is named `windows-dual-audio-recorder` in `Cargo.toml`, but the product surface is **JobServe** — a desktop UI shell for a job-search assistant. The repo is mid-pivot: the original product was a dual-stream audio recorder; the UI shell layered on top is the current focus, and the recorder backend is kept available but **not wired into the GUI**.

Three modules, with sharply different concerns:

- **`src/main.rs`** — CLI entry point and dispatcher. Parses `Args` with clap, then branches:
  - no args, or `--gui` → `gui::run_gui` (UI path; ignores recording flags today via `UiSeed`)
  - `--list-devices` → `recorder::query_device_catalog` + print
  - otherwise → headless dual-recording loop driven by `recorder::RecordingSession`, with a Ctrl+C handler flipping a shared `AtomicBool`.

- **`src/recorder.rs`** — WASAPI capture backend. Independent of the UI. Key shape:
  - `RecordingSession::start` spawns one capture worker thread per source (system audio via render-loopback, microphone via capture). Each worker initializes COM (MTA) on its own thread (`ComGuard` calls `wasapi::deinitialize` on drop), resolves a `Device`, opens an event-driven shared-mode stream, and pumps packets into a `hound::WavWriter`.
  - All samples are decoded to `f32` and written as 32-bit float WAV regardless of the device's native format (`write_packet_as_f32` / `decode_sample_to_f32` handle 8/16/24/32-bit PCM and 32-bit float).
  - Coordination is a single `Arc<AtomicBool>` stop flag shared by the session and both workers; if either worker errors, it sets the flag so the other stops too.
  - Public API surface used by `main.rs`: `RecorderConfig`, `RecordingSession`, `RecordingReport`, `query_device_catalog`, `print_device_catalog`, `print_capture_summary`, `DEFAULT_POLL_TIMEOUT_MS`.

- **`src/gui.rs`** — `eframe`/`egui` desktop shell, ~1.4k lines. Single `RecorderApp` struct holds all UI state. Three sections (`Chat`, `Timeline`, `Settings`) selected via a left icon rail; the chat section also has a collapsible folder/history sidebar. The `eframe::App` impl dispatches to `draw_icon_rail`, optionally `draw_chat_sidebar`, then one of `draw_chat_workspace` / `draw_timeline_workspace` / `draw_settings_workspace`. `apply_theme` configures the dark palette in `RecorderApp::new`.

  **Important:** all chat threads, timeline entries, resumes, custom info, and settings shown in the UI are **mock state** seeded by `sample_chat_folders`, `sample_timeline_entries`, and `sample_settings`. Nothing persists across launches, no LLM calls are made, and API keys entered into Settings are kept only in memory. Treat this layer as a UI prototype, not a working product, until persistence and model wiring are added.

  `UiSeed::from_args` currently ignores all CLI args — the GUI does not yet honor `--out-dir`, `--system-file`, etc., despite the README example showing `cargo run -- --gui --out-dir ...`.

## Things that will trip you up

- The recorder backend and the GUI **do not talk to each other**. There is no "start recording" button in the UI. If a task asks to surface the recorder in the GUI, that's new wiring across `gui.rs` ↔ `recorder.rs`, including running the WASAPI session off the UI thread.
- COM must be initialized per thread (`init_mta`) before any WASAPI call. Don't move device enumeration or capture work onto a thread that hasn't done this.
- `query_device_catalog` deliberately runs enumeration on a freshly-spawned thread so its COM init does not poison the caller's thread. Preserve that pattern if you add similar queries.
- Output is always 32-bit float WAV. If you change the writer spec in `make_wav_spec`, also update `write_packet_as_f32` — the two are tightly coupled.
- `target-ui-check/` and `target-ui-checkZrGiFK/` are alternate cargo target dirs left behind from previous runs; safe to ignore, not part of the build.
