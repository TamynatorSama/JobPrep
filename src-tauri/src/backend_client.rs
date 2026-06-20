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
//! Every event payload is `{ "streamId": "<id>", "content": <value> }` — the
//! `streamId` (minted by the frontend per stream and passed into the start
//! command) lets the UI run several streams at once and route each event to the
//! right thread. `content` is the raw string for token/log/error, the JSON
//! object for scorecard, and `null` for `done`.

use std::io::{BufRead, BufReader};

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

/// Payload shape the Tauri command receives for each master resume before
/// forwarding it to the Python backend. Mirrors the `MasterResume` pydantic
/// model. `docx_b64` is the raw .docx bytes when available, used by the
/// backend as the styling base for in-place section editing.
#[derive(Debug, Clone, Deserialize)]
pub struct MasterResumePayload {
    pub name: String,
    pub text: String,
    #[serde(default)]
    pub docx_b64: Option<String>,
}

/// One document from a job's corpus, forwarded to the backend for RAG
/// retrieval. Mirrors the `RagDoc` pydantic model. `source` is a short label
/// ("resume", "company_research", "chat: <title>") shown to the LLM.
#[derive(Debug, Clone, Deserialize)]
pub struct RagDocPayload {
    pub source: String,
    pub text: String,
}

const EV_TOKEN:           &str = "chat:token";
const EV_LOG:             &str = "chat:log";
const EV_DONE:            &str = "chat:done";
const EV_ERROR:           &str = "chat:error";
const EV_SCORECARD:       &str = "chat:scorecard";       // application-tailor ATS scorecard JSON
const EV_RESUME_DOCX:     &str = "chat:resume_docx";     // base64-encoded .docx bytes
const EV_TAILORED_RESUME: &str = "chat:tailored_resume"; // plain-text tailored resume

/// Emit one stream event to the frontend, tagged with the originating
/// `stream_id` so several concurrent streams can be told apart. Payload:
/// `{ "streamId": "<id>", "content": <value> }`.
fn emit_ev(app: &AppHandle, event: &str, stream_id: &str, content: Value) -> tauri::Result<()> {
    app.emit(event, serde_json::json!({ "streamId": stream_id, "content": content }))
}

/// Sends a chat message and forwards the resulting SSE stream to the
/// frontend. Runs on its own thread so the Tauri command returns instantly.
/// `llm` is the provider config (Settings toggle + per-provider keys) built
/// by `Credentials::llm_json`.
#[allow(clippy::too_many_arguments)]
pub fn stream_chat(
    app: AppHandle,
    base_url: String,
    message: String,
    job_context: String,
    history: Vec<(String, String)>,
    mode: String,
    llm: Value,
    documents: Vec<RagDocPayload>,
    stream_id: String,
) {
    let url = format!("{base_url}/chat/stream");
    let history_json: Vec<_> = history
        .into_iter()
        .map(|(role, content)| serde_json::json!({ "role": role, "content": content }))
        .collect();
    let documents_json: Vec<_> = documents
        .into_iter()
        .map(|d| serde_json::json!({ "source": d.source, "text": d.text }))
        .collect();
    let body = serde_json::json!({
        "message":     message,
        "job_context": job_context,
        "history":     history_json,
        "mode":        mode,
        "llm":         llm,
        "documents":   documents_json,
    });
    spawn_stream(app, url, body, stream_id);
}

/// JD-focused role-fit analysis stream.
pub fn stream_research(
    app: AppHandle,
    base_url: String,
    company: String,
    role: String,
    jd: String,
    llm: Value,
    stream_id: String,
) {
    let url = format!("{base_url}/research/stream");
    let body = serde_json::json!({
        "company":         company,
        "role":            role,
        "job_description": jd,
        "llm":             llm,
    });
    spawn_stream(app, url, body, stream_id);
}

/// Full company research — the embedded research_scraper engine. The `llm`
/// config carries every provider key; the backend maps the selected provider
/// onto the engine's primary lane and the spares onto fallback lanes.
#[allow(clippy::too_many_arguments)]
pub fn stream_company_research(
    app: AppHandle,
    base_url: String,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    llm: Value,
    stream_id: String,
) {
    let url = format!("{base_url}/company-research/stream");
    let body = serde_json::json!({
        "company":         company,
        "role":            role,
        "location":        location,
        "job_description": job_description,
        "tailored_resume": tailored_resume,
        "llm":             llm,
    });
    spawn_stream(app, url, body, stream_id);
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
    master_resumes: Vec<MasterResumePayload>,
    llm: Value,
    stream_id: String,
) {
    let url = format!("{base_url}/application/tailor-resume");
    let resumes_json: Vec<_> = master_resumes
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "name":     r.name,
                "text":     r.text,
                "docx_b64": r.docx_b64,
            })
        })
        .collect();
    let body = serde_json::json!({
        "company":         company,
        "role":            role,
        "location":        location,
        "job_description": job_description,
        "master_resumes":  resumes_json,
        "llm":             llm,
    });
    spawn_stream(app, url, body, stream_id);
}

/// Simulate a recruiter knockout screen for a job using the tailored resume.
#[allow(clippy::too_many_arguments)]
pub fn stream_knockout_screen(
    app: AppHandle,
    base_url: String,
    company: String,
    role: String,
    location: String,
    job_description: String,
    tailored_resume: String,
    llm: Value,
    stream_id: String,
) {
    let url = format!("{base_url}/application/knockout-screen");
    let body = serde_json::json!({
        "company":         company,
        "role":            role,
        "location":        location,
        "job_description": job_description,
        "tailored_resume": tailored_resume,
        "llm":             llm,
    });
    spawn_stream(app, url, body, stream_id);
}

// ─── Extension bridge ────────────────────────────────────────────────────────

/// POST /config/seed — refresh the backend's in-memory LLM config (provider
/// toggle + every provider key). Sends the shared secret so the token-guarded
/// endpoint accepts it. Called on startup and whenever the user saves
/// Settings (no app restart needed).
pub fn seed_config(base_url: &str, token: &str, llm: &Value) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{base_url}/config/seed"))
        .header("X-InterPrep-Token", token)
        .json(&serde_json::json!({ "llm": llm }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("seed_config failed: HTTP {}", resp.status()))
    }
}

/// GET /inbox/jobs — JD captures queued by the browser extension.
pub fn inbox_pending(base_url: &str, token: &str) -> Result<Value, String> {
    let client = reqwest::blocking::Client::new();
    client
        .get(format!("{base_url}/inbox/jobs"))
        .header("X-InterPrep-Token", token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .and_then(|r| r.json::<Value>())
        .map_err(|e| e.to_string())
}

/// POST /inbox/ack — drop consumed captures off the queue.
pub fn inbox_ack(base_url: &str, token: &str, ids: Vec<String>) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{base_url}/inbox/ack"))
        .header("X-InterPrep-Token", token)
        .json(&serde_json::json!({ "ids": ids }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() { Ok(()) } else { Err(format!("inbox_ack failed: HTTP {}", resp.status())) }
}

/// GET /inbox/timeline — "mark Applied + note" events queued by the extension
/// after an autofill finishes. The app applies each to the job's stage timeline.
pub fn timeline_pending(base_url: &str, token: &str) -> Result<Value, String> {
    let client = reqwest::blocking::Client::new();
    client
        .get(format!("{base_url}/inbox/timeline"))
        .header("X-InterPrep-Token", token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .and_then(|r| r.json::<Value>())
        .map_err(|e| e.to_string())
}

/// POST /inbox/timeline/ack — drop applied timeline events off the queue.
pub fn timeline_ack(base_url: &str, token: &str, ids: Vec<String>) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{base_url}/inbox/timeline/ack"))
        .header("X-InterPrep-Token", token)
        .json(&serde_json::json!({ "ids": ids }))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| e.to_string())?;
    if resp.status().is_success() { Ok(()) } else { Err(format!("timeline_ack failed: HTTP {}", resp.status())) }
}

// ─── Voice (blocking request/response, not SSE) ──────────────────────────────

/// POST /cheatsheet/build — aggregate a job's context into a structured
/// interview cheatsheet (+ maintained markdown). Non-streaming JSON; the LLM
/// call can be slow on the smart tier, so allow a generous timeout.
pub fn build_cheatsheet(base_url: &str, payload: Value) -> Result<Value, String> {
    let client = reqwest::blocking::Client::new();
    let resp: Value = client
        .post(format!("{base_url}/cheatsheet/build"))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .and_then(|r| r.json::<Value>())
        .map_err(|e| e.to_string())?;
    if let Some(err) = resp["error"].as_str() {
        return Err(err.to_string());
    }
    Ok(resp)
}

/// GET /voice/status — capability + device (cuda/cpu) report.
pub fn voice_status(base_url: &str) -> Result<Value, String> {
    let client = reqwest::blocking::Client::new();
    client
        .get(format!("{base_url}/voice/status"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .and_then(|r| r.json::<Value>())
        .map_err(|e| e.to_string())
}

/// POST /voice/tts — synthesize `text`, returning decoded WAV bytes.
pub fn voice_tts(base_url: &str, text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let client = reqwest::blocking::Client::new();
    let resp: Value = client
        .post(format!("{base_url}/voice/tts"))
        .json(&serde_json::json!({ "text": text }))
        // First call also loads the model — allow generous time.
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .and_then(|r| r.json::<Value>())
        .map_err(|e| e.to_string())?;
    if let Some(err) = resp["error"].as_str() {
        return Err(err.to_string());
    }
    let b64 = resp["audio_b64"].as_str().ok_or("no audio in TTS response")?;
    base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("base64 decode failed: {e}"))
}

/// POST /voice/tts_stream — synthesize `text`, returning the streaming HTTP
/// response (raw LE 16-bit mono PCM body) and the sample rate from the
/// `X-Sample-Rate` header. The caller reads the body incrementally so playback
/// can start on the first chunk instead of waiting for the whole sentence.
pub fn voice_tts_stream(
    base_url: &str,
    text: &str,
    engine: &str,
    speaker: &str,
) -> Result<(u32, reqwest::blocking::Response), String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(format!("{base_url}/voice/tts_stream"))
        .json(&serde_json::json!({ "text": text, "engine": engine, "speaker": speaker }))
        // First call also loads the model — allow generous time to first byte.
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("tts_stream HTTP {}", resp.status()));
    }
    let sr = resp
        .headers()
        .get("x-sample-rate")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(16000);
    Ok((sr, resp))
}

/// POST /voice/stt — transcribe base64-encoded WAV, returning the text.
pub fn voice_stt(base_url: &str, wav_b64: &str) -> Result<String, String> {
    let client = reqwest::blocking::Client::new();
    let resp: Value = client
        .post(format!("{base_url}/voice/stt"))
        .json(&serde_json::json!({ "audio_b64": wav_b64 }))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .and_then(|r| r.json::<Value>())
        .map_err(|e| e.to_string())?;
    if let Some(err) = resp["error"].as_str() {
        return Err(err.to_string());
    }
    Ok(resp["text"].as_str().unwrap_or("").to_string())
}

/// POST /voice/warm — kick the engine's cold-start warmup. Returns once the
/// request is accepted; the warmup proceeds on the sidecar in the background.
pub fn voice_warm(base_url: &str, engine: &str, speaker: &str) -> Result<(), String> {
    let client = reqwest::blocking::Client::new();
    client
        .post(format!("{base_url}/voice/warm"))
        .json(&serde_json::json!({ "text": "", "engine": engine, "speaker": speaker }))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn spawn_stream(app: AppHandle, url: String, body: Value, stream_id: String) {
    std::thread::spawn(move || {
        let sid = stream_id.as_str();
        eprintln!("[stream {sid}] POST {url}");
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
                eprintln!("[stream {sid}] HTTP error {status}: {body}");
                let _ = emit_ev(&app, EV_ERROR, sid, Value::String(format!("HTTP {status}: {body}")));
                return;
            }
            Err(e) => {
                eprintln!("[stream {sid}] reqwest error: {e}");
                let _ = emit_ev(&app, EV_ERROR, sid, Value::String(e.to_string()));
                return;
            }
        };

        let mut sse_event_count: usize = 0;
        let reader = BufReader::new(resp);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    let _ = emit_ev(&app, EV_ERROR, sid, Value::String(e.to_string()));
                    return;
                }
            };
            if line.is_empty() {
                continue;
            }
            let Some(data) = line.strip_prefix("data: ") else { continue; };

            // OpenAI-style terminator.
            if data.trim() == "[DONE]" {
                let _ = emit_ev(&app, EV_DONE, sid, Value::Null);
                return;
            }

            let Ok(val) = serde_json::from_str::<Value>(data) else {
                // Non-JSON payload — best-effort treat as a token.
                let _ = emit_ev(&app, EV_TOKEN, sid, Value::String(data.to_string()));
                continue;
            };

            sse_event_count += 1;
            let kind = val["type"].as_str().unwrap_or("?");
            if sse_event_count <= 5 || sse_event_count % 20 == 0 {
                eprintln!("[stream {sid}] event#{sse_event_count} type={kind}");
            }

            match val["type"].as_str() {
                Some("token") => {
                    if let Some(content) = val["content"].as_str() {
                        if let Err(e) = emit_ev(&app, EV_TOKEN, sid, Value::String(content.to_string())) {
                            eprintln!("[stream {sid}] emit token failed: {e}");
                            return;
                        }
                    }
                }
                Some("stage") => {
                    if let Some(content) = val["content"].as_str() {
                        if let Err(e) = emit_ev(&app, EV_LOG, sid, Value::String(content.to_string())) {
                            eprintln!("[stream {sid}] emit log failed: {e}");
                        }
                    }
                }
                Some("scorecard") => {
                    // Forward the JSON object as-is so the frontend can format it.
                    let _ = emit_ev(&app, EV_SCORECARD, sid, val["content"].clone());
                }
                Some("resume_docx") => {
                    if let Some(b64) = val["content"].as_str() {
                        let _ = emit_ev(&app, EV_RESUME_DOCX, sid, Value::String(b64.to_string()));
                    }
                }
                Some("tailored_resume") => {
                    if let Some(content) = val["content"].as_str() {
                        let _ = emit_ev(&app, EV_TAILORED_RESUME, sid, Value::String(content.to_string()));
                    }
                }
                Some("done") => {
                    let _ = emit_ev(&app, EV_DONE, sid, Value::Null);
                    return;
                }
                Some("error") => {
                    let msg = val["content"].as_str().unwrap_or("Unknown error");
                    let _ = emit_ev(&app, EV_ERROR, sid, Value::String(msg.to_string()));
                    return;
                }
                _ => {}
            }
        }

        // Server closed the stream without an explicit `done`.
        let _ = emit_ev(&app, EV_DONE, sid, Value::Null);
    });
}
