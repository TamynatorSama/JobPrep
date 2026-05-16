//! InterPrep — Tauri runtime entry point.

mod backend_client;
mod credentials;
mod jobs_store;
mod sidecar;
mod types;

use std::sync::Mutex;

use credentials::Credentials;
use sidecar::PythonSidecar;
use tauri::{AppHandle, Emitter, Manager, State};
use types::Job;

/// Backend lifecycle state. Held in `tauri::State` so commands can ask for
/// the current URL and the setup hook can publish it once startup finishes.
#[derive(Default)]
struct SidecarState {
    inner: Mutex<SidecarInner>,
}

#[derive(Default)]
struct SidecarInner {
    sidecar:   Option<PythonSidecar>,
    base_url:  Option<String>,
    /// Set when startup fails; surfaced to the frontend on demand.
    last_err:  Option<String>,
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

// ─── Backend lifecycle ────────────────────────────────────────────────────

/// `"starting"` | `"ready"` | `"failed"`. Frontend polls this once on mount
/// in case it missed the `sidecar:ready` / `sidecar:error` events (e.g. the
/// setup hook finished before the page subscribed).
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

// ─── Chat ─────────────────────────────────────────────────────────────────

/// Kicks off an SSE chat stream. Returns immediately; events stream back via
/// `chat:token` / `chat:log` / `chat:done` / `chat:error` window events.
#[tauri::command]
fn start_chat_stream(
    app: AppHandle,
    state: State<SidecarState>,
    message: String,
    job_context: String,
    api_key: String,
) -> Result<(), String> {
    let url = state
        .inner
        .lock()
        .unwrap()
        .base_url
        .clone()
        .ok_or_else(|| "Backend is not ready yet".to_string())?;
    backend_client::stream_chat(app, url, message, job_context, api_key);
    Ok(())
}

// ─── Entry ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(SidecarState::default())
        .setup(|app| {
            // Boot the Python sidecar in the background so the window opens
            // immediately. Result is broadcast to the frontend as an event
            // and stored in state so late-joining listeners can ask.
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
            backend_status,
            start_chat_stream,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
