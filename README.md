# JobServe Desktop Prototype

A Windows desktop prototype in Rust with:

- a dark job-assistant UI shell for `Chat`, `Timeline`, and `Settings`
- grouped chat folders with an expandable history sidebar
- settings forms for API keys, resumes, and reusable candidate information
- the earlier WASAPI-based system-audio and microphone recording backend still available in code and CLI mode

## What This Starter Does

- opens a native dark-mode desktop UI with a slim icon rail
- shows a chat workspace with grouped folders, history, and a message composer
- shows a timeline workspace for application milestones
- shows a settings workspace for API keys, resumes, and custom context
- keeps the earlier audio-capture backend and headless CLI utilities available

## Usage

Launch the desktop app:

```powershell
cargo run
```

This opens the `JobServe` UI shell.

List devices:

```powershell
cargo run -- --list-devices
```

Record for 30 seconds:

```powershell
cargo run -- --seconds 30
```

Record until you stop it:

```powershell
cargo run -- --seconds 0
```

Choose specific device names from `--list-devices`:

```powershell
cargo run -- --seconds 20 --system-device "Speakers (Realtek(R) Audio)" --mic-device "Microphone (USB Audio Device)"
```

Write files into a custom folder:

```powershell
cargo run -- --seconds 15 --out-dir captures
```

Open the UI with prefilled values:

```powershell
cargo run -- --gui --out-dir captures --system-file desktop.wav --mic-file mic.wav
```

## Output

By default the app writes:

- `captures/system_audio.wav`
- `captures/microphone.wav`

## Notes

- This starter is Windows-only.
- The current chat, timeline, and settings content is local mock state meant to shape the product UI before wiring persistence and real model calls.
- The audio backend is still present for system-audio and microphone capture in headless mode.
- If you later want a mixed track from the audio backend, you can mix the two WAV files after recording or extend the recorder path to add a mixer stage.

## Build

You need a Rust toolchain installed locally:

```powershell
rustup default stable
cargo build
```
