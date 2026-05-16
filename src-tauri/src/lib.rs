//! InterPrep — Tauri runtime entry point.

mod backend_client;
mod credentials;
mod jobs_store;
mod recorder;
mod resume_store;
mod sidecar;
mod types;

use std::path::PathBuf;
use std::sync::Mutex;

use credentials::Credentials;
use resume_store::Resume;
use sidecar::PythonSidecar;
use tauri::{AppHandle, Emitter, Manager, State};
use types::Job;

// ─── State ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct SidecarState {
    inner: Mutex<SidecarInner>,
}

#[derive(Default)]
struct SidecarInner {
    sidecar:  Option<PythonSidecar>,
    base_url: Option<String>,
    last_err: Option<String>,
}

#[derive(Default)]
struct RecorderState {
    inner: Mutex<Option<recorder::RecordingSession>>,
}

// ─── Jobs ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn list_jobs() -> Vec<Job> {
    jobs_store::load()
}

#[tauri::command]
fn save_jobs(jobs: Vec<Job>) -> Result<(), String> {
    jobs_store::save(&jobs)
}

// ─── Credentials ──────────────────────────────────────────────────────────

#[tauri::command]
fn load_credentials() -> Credentials {
    Credentials::load()
}

#[tauri::command]
fn save_credentials(credentials: Credentials) -> Result<(), String> {
    credentials.save()
}

// ─── Resumes ──────────────────────────────────────────────────────────────

#[tauri::command]
fn list_resumes() -> Vec<Resume> {
    resume_store::load()
}

#[tauri::command]
fn save_resumes(resumes: Vec<Resume>) -> Result<(), String> {
    resume_store::save(&resumes)
}

// ─── Backend lifecycle ────────────────────────────────────────────────────

#[tauri::command]
fn backend_status(state: State<SidecarState>) -> serde_json::Value {
    let inner = state.inner.lock().unwrap();
    if inner.base_url.is_some() {
        serde_json::json!({ "status": "ready", "url": inner.base_url })
    } else if let Some(err) = &inner.last_err {
        serde_json::json!({ "status": "failed", "error": err })
    } else {
        serde_json::json!({ "status": "starting" })
    }
}

// ─── Streams (chat / research / company research) ─────────────────────────

fn require_url(state: &State<SidecarState>) -> Result<String, String> {
    state
        .inner
        .lock()
        .unwrap()
        .base_url
        .clone()
        .ok_or_else(|| "Backend is not ready yet".to_string())
}

#[tauri::command]
fn start_chat_stream(
    app: AppHandle,
    state: State<SidecarState>,
    message: String,
    job_context: String,
    api_key: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_chat(app, url, message, job_context, api_key);
    Ok(())
}

#[tauri::command]
fn start_research_stream(
    app: AppHandle,
    state: State<SidecarState>,
    company: String,
    role: String,
    job_description: String,
    api_key: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_research(app, url, company, role, job_description, api_key);
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn start_company_research_stream(
    app: AppHandle,
    state: State<SidecarState>,
    company: String,
    role: String,
    location: String,
    job_description: String,
    api_key: String,
    glassdoor_email: String,
    glassdoor_password: String,
    indeed_email: String,
    indeed_password: String,
) -> Result<(), String> {
    let url = require_url(&state)?;
    backend_client::stream_company_research(
        app,
        url,
        company,
        role,
        location,
        job_description,
        api_key,
        glassdoor_email,
        glassdoor_password,
        indeed_email,
        indeed_password,
    );
    Ok(())
}

// ─── Audio recording ──────────────────────────────────────────────────────

#[tauri::command]
fn list_audio_devices() -> Result<serde_json::Value, String> {
    let catalog =
        recorder::query_device_catalog().map_err(|e| format!("device query failed: {e}"))?;
    let to_json = |entries: &[recorder::AudioDeviceEntry]| -> Vec<serde_json::Value> {
        entries
            .iter()
            .map(|d| serde_json::json!({ "name": d.name, "isDefault": d.is_default }))
            .collect()
    };
    Ok(serde_json::json!({
        "render":  to_json(&catalog.render_devices),
        "capture": to_json(&catalog.capture_devices),
    }))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartRecordingArgs {
    /// Directory where the WAV files will be written. Defaults to
    /// `%USERPROFILE%\InterPrep Recordings` if blank.
    #[serde(default)]
    out_dir: String,
    /// Override the default playback device. Blank = Windows default.
    #[serde(default)]
    system_device: String,
    /// Override the default microphone. Blank = Windows default.
    #[serde(default)]
    mic_device: String,
    /// File names. Defaults to `system_audio.wav` / `microphone.wav`.
    #[serde(default)]
    system_file: String,
    #[serde(default)]
    mic_file: String,
}

#[tauri::command]
fn start_recording(
    state: State<RecorderState>,
    args: StartRecordingArgs,
) -> Result<serde_json::Value, String> {
    let mut slot = state.inner.lock().unwrap();
    if slot.is_some() {
        return Err("A recording is already in progress".into());
    }

    let out_dir = if args.out_dir.is_empty() {
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join("InterPrep Recordings")
    } else {
        PathBuf::from(&args.out_dir)
    };

    let config = recorder::RecorderConfig {
        system_device_name: opt(args.system_device),
        mic_device_name:    opt(args.mic_device),
        output_dir:         out_dir.clone(),
        system_file:        non_empty(args.system_file, "system_audio.wav"),
        mic_file:           non_empty(args.mic_file,    "microphone.wav"),
        poll_timeout_ms:    recorder::DEFAULT_POLL_TIMEOUT_MS,
    };

    let session = recorder::RecordingSession::start(config, None)
        .map_err(|e| format!("start failed: {e:#}"))?;
    *slot = Some(session);

    Ok(serde_json::json!({
        "outDir":     out_dir,
        "systemFile": "system_audio.wav",
        "micFile":    "microphone.wav",
    }))
}

#[tauri::command]
fn stop_recording(state: State<RecorderState>) -> Result<serde_json::Value, String> {
    let session = state
        .inner
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "No active recording".to_string())?;
    session.request_stop();
    let report = session.finish().map_err(|e| format!("stop failed: {e:#}"))?;
    Ok(serde_json::json!({
        "system": capture_to_json(&report.system),
        "mic":    capture_to_json(&report.microphone),
    }))
}

fn capture_to_json(c: &recorder::CaptureSummary) -> serde_json::Value {
    serde_json::json!({
        "path":            c.output_path,
        "deviceName":      c.device_name,
        "sampleRate":      c.sample_rate,
        "channels":        c.channels,
        "framesWritten":   c.frames_written,
        "durationSeconds": c.duration_seconds(),
    })
}

fn opt(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

fn non_empty(s: String, fallback: &str) -> String {
    if s.trim().is_empty() { fallback.to_string() } else { s }
}

// ─── Entry ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(SidecarState::default())
        .manage(RecorderState::default())
        .setup(|app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let state = handle.state::<SidecarState>();
                match PythonSidecar::start() {
                    Ok(sidecar) => {
                        let url = sidecar.base_url();
                        {
                            let mut inner = state.inner.lock().unwrap();
                            inner.base_url = Some(url.clone());
                            inner.sidecar  = Some(sidecar);
                            inner.last_err = None;
                        }
                        let _ = handle.emit("sidecar:ready", url);
                    }
                    Err(e) => {
                        {
                            let mut inner = state.inner.lock().unwrap();
                            inner.last_err = Some(e.clone());
                        }
                        let _ = handle.emit("sidecar:error", e);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_jobs,
            save_jobs,
            load_credentials,
            save_credentials,
            list_resumes,
            save_resumes,
            backend_status,
            start_chat_stream,
            start_research_stream,
            start_company_research_stream,
            list_audio_devices,
            start_recording,
            stop_recording,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
