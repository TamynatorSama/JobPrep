//! Streams chat / research output from the Python sidecar over SSE and
//! forwards each event to the Tauri frontend as a window event.
//!
//! The Python backend speaks the following SSE protocol (one JSON object per
//! `data:` line):
//!
//!   `{ "type": "token", "content": "hello" }`   — append to current bubble
//!   `{ "type": "stage", "content": "Coordinator deciding next step…" }`
//!                                                — progress log, render in
//!                                                  the "Thinking" disclosure
//!   `{ "type": "done"  }`                        — stream complete
//!   `{ "type": "error","content": "…" }`         — fatal error
//!
//! Frontend listens for `chat:token`, `chat:log`, `chat:done`, `chat:error`.
//! Payload for token/log/error is the raw `content` string; `done` carries
//! the empty string.

use std::io::{BufRead, BufReader};

use serde_json::Value;
use tauri::{AppHandle, Emitter};

const EV_TOKEN: &str = "chat:token";
const EV_LOG:   &str = "chat:log";
const EV_DONE:  &str = "chat:done";
const EV_ERROR: &str = "chat:error";

/// Sends a chat message and forwards the resulting SSE stream to the
/// frontend. Runs on its own thread so the Tauri command returns instantly.
pub fn stream_chat(
    app: AppHandle,
    base_url: String,
    message: String,
    job_context: String,
    api_key: String,
) {
    let url = format!("{base_url}/chat/stream");
    let body = serde_json::json!({
        "message":     message,
        "job_context": job_context,
        "api_key":     api_key,
    });
    spawn_stream(app, url, body);
}

fn spawn_stream(app: AppHandle, url: String, body: Value) {
    std::thread::spawn(move || {
        let client = reqwest::blocking::Client::new();
        let resp = match client
            .post(&url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(300))
            .send()
        {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                let status = r.status();
                let body = r.text().unwrap_or_default();
                let _ = app.emit(EV_ERROR, format!("HTTP {status}: {body}"));
                return;
            }
            Err(e) => {
                let _ = app.emit(EV_ERROR, e.to_string());
                return;
            }
        };

        let reader = BufReader::new(resp);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    let _ = app.emit(EV_ERROR, e.to_string());
                    return;
                }
            };
            if line.is_empty() {
                continue;
            }
            let Some(data) = line.strip_prefix("data: ") else { continue; };

            // OpenAI-style terminator.
            if data.trim() == "[DONE]" {
                let _ = app.emit(EV_DONE, "");
                return;
            }

            let Ok(val) = serde_json::from_str::<Value>(data) else {
                // Non-JSON payload — best-effort treat as a token.
                let _ = app.emit(EV_TOKEN, data.to_string());
                continue;
            };

            match val["type"].as_str() {
                Some("token") => {
                    if let Some(content) = val["content"].as_str() {
                        if app.emit(EV_TOKEN, content.to_string()).is_err() {
                            return;
                        }
                    }
                }
                Some("stage") => {
                    if let Some(content) = val["content"].as_str() {
                        let _ = app.emit(EV_LOG, content.to_string());
                    }
                }
                Some("done") => {
                    let _ = app.emit(EV_DONE, "");
                    return;
                }
                Some("error") => {
                    let msg = val["content"].as_str().unwrap_or("Unknown error");
                    let _ = app.emit(EV_ERROR, msg.to_string());
                    return;
                }
                _ => {}
            }
        }

        // Server closed the stream without an explicit `done`.
        let _ = app.emit(EV_DONE, "");
    });
}
