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

const EV_TOKEN:           &str = "chat:token";
const EV_LOG:             &str = "chat:log";
const EV_DONE:            &str = "chat:done";
const EV_ERROR:           &str = "chat:error";
const EV_SCORECARD:       &str = "chat:scorecard";       // application-tailor ATS scorecard JSON
const EV_RESUME_DOCX:     &str = "chat:resume_docx";     // base64-encoded .docx bytes
const EV_TAILORED_RESUME: &str = "chat:tailored_resume"; // plain-text tailored resume

/// Sends a chat message and forwards the resulting SSE stream to the
/// frontend. Runs on its own thread so the Tauri command returns instantly.
pub fn stream_chat(
    app: AppHandle,
    base_url: String,
    message: String,
    job_context: String,
    history: Vec<(String, String)>,
    mode: String,
    api_key: String,
) {
    let url = format!("{base_url}/chat/stream");
    let history_json: Vec<_> = history
        .into_iter()
        .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
        .collect();
    let body = serde_json::json!({
        "message":     message,
        "job_context": job_context,
        "history":     history_json,
        "mode":        mode,
        "api_key":     api_key,
    });
    spawn_stream(app, url, body);
}

/// JD-focused role-fit analysis stream.
pub fn stream_research(
    app: AppHandle,
    base_url: String,
    company: String,
    role: String,
    jd: String,
    api_key: String,
) {
    let url = format!("{base_url}/research/stream");
    let body = serde_json::json!({
        "company":         company,
        "role":            role,
        "job_description": jd,
        "api_key":         api_key,
    });
    spawn_stream(app, url, body);
}

/// Full company research — runs the scraping agents that need the user's
/// Glassdoor / Indeed logins to bypass paywalls.
#[allow(clippy::too_many_arguments)]
pub fn stream_company_research(
    app: AppHandle,
    base_url: String,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    api_key: String,
    glassdoor_email: String,
    glassdoor_password: String,
    indeed_email: String,
    indeed_password: String,
) {
    let url = format!("{base_url}/company-research/stream");
    let body = serde_json::json!({
        "company":            company,
        "role":               role,
        "location":           location,
        "job_description":    job_description,
        "tailored_resume":    tailored_resume,
        "api_key":            api_key,
        "glassdoor_email":    glassdoor_email,
        "glassdoor_password": glassdoor_password,
        "indeed_email":       indeed_email,
        "indeed_password":    indeed_password,
    });
    spawn_stream(app, url, body);
}

/// Tailor a resume + cover letter for a specific job. Streams cover-letter
/// tokens to `chat:token`, the scorecard JSON to `chat:scorecard`, the
/// base64-encoded .docx to `chat:resume_docx`, and the plain-text tailored
/// resume to `chat:tailored_resume`.
#[allow(clippy::too_many_arguments)]
pub fn stream_application_tailor(
    app: AppHandle,
    base_url: String,
    company: String,
    role: String,
    location: String,
    job_description: String,
    master_resumes: Vec<(String, String)>,
    api_key: String,
) {
    let url = format!("{base_url}/application/tailor-resume");
    let resumes_json: Vec<_> = master_resumes
        .into_iter()
        .map(|(name, text)| serde_json::json!({ "name": name, "text": text }))
        .collect();
    let body = serde_json::json!({
        "company":         company,
        "role":            role,
        "location":        location,
        "job_description": job_description,
        "master_resumes":  resumes_json,
        "api_key":         api_key,
    });
    spawn_stream(app, url, body);
}

/// Simulate a recruiter knockout screen for a job using the tailored resume.
pub fn stream_knockout_screen(
    app: AppHandle,
    base_url: String,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    api_key: String,
) {
    let url = format!("{base_url}/application/knockout-screen");
    let body = serde_json::json!({
        "company":         company,
        "role":            role,
        "location":        location,
        "job_description": job_description,
        "tailored_resume": tailored_resume,
        "api_key":         api_key,
    });
    spawn_stream(app, url, body);
}

fn spawn_stream(app: AppHandle, url: String, body: Value) {
    std::thread::spawn(move || {
        eprintln!("[stream] POST {url}");
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
                eprintln!("[stream] HTTP error {status}: {body}");
                let _ = app.emit(EV_ERROR, format!("HTTP {status}: {body}"));
                return;
            }
            Err(e) => {
                eprintln!("[stream] reqwest error: {e}");
                let _ = app.emit(EV_ERROR, e.to_string());
                return;
            }
        };

        let mut sse_event_count: usize = 0;
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

            sse_event_count += 1;
            let kind = val["type"].as_str().unwrap_or("?");
            if sse_event_count <= 5 || sse_event_count % 20 == 0 {
                eprintln!("[stream] event#{sse_event_count} type={kind}");
            }

            match val["type"].as_str() {
                Some("token") => {
                    if let Some(content) = val["content"].as_str() {
                        if let Err(e) = app.emit(EV_TOKEN, content.to_string()) {
                            eprintln!("[stream] emit token failed: {e}");
                            return;
                        }
                    }
                }
                Some("stage") => {
                    if let Some(content) = val["content"].as_str() {
                        if let Err(e) = app.emit(EV_LOG, content.to_string()) {
                            eprintln!("[stream] emit log failed: {e}");
                        }
                    }
                }
                Some("scorecard") => {
                    // Forward the JSON object as-is so the frontend can format it.
                    let _ = app.emit(EV_SCORECARD, val["content"].clone());
                }
                Some("resume_docx") => {
                    if let Some(b64) = val["content"].as_str() {
                        let _ = app.emit(EV_RESUME_DOCX, b64.to_string());
                    }
                }
                Some("tailored_resume") => {
                    if let Some(content) = val["content"].as_str() {
                        let _ = app.emit(EV_TAILORED_RESUME, content.to_string());
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
