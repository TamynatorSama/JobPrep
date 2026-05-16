//! InterPrep — Tauri runtime entry point.

mod credentials;
mod jobs_store;
mod types;

use credentials::Credentials;
use types::Job;

// ─── Jobs ─────────────────────────────────────────────────────────────────

/// Returns every persisted job (active + archived). Empty on first run.
#[tauri::command]
fn list_jobs() -> Vec<Job> {
    jobs_store::load()
}

/// Atomically persists the full jobs list. The frontend owns state and
/// calls this after any mutation (create / archive / unarchive / delete).
#[tauri::command]
fn save_jobs(jobs: Vec<Job>) -> Result<(), String> {
    jobs_store::save(&jobs)
}

// ─── Credentials ──────────────────────────────────────────────────────────

/// Reads every credential field from Windows Credential Manager. Missing
/// fields come back as empty strings — there's no "never set" sentinel.
#[tauri::command]
fn load_credentials() -> Credentials {
    Credentials::load()
}

/// Writes the bundle. Empty fields delete the matching keyring entry so
/// clearing a field in the UI really removes it from the OS.
#[tauri::command]
fn save_credentials(credentials: Credentials) -> Result<(), String> {
    credentials.save()
}

// ─── Entry ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_jobs,
            save_jobs,
            load_credentials,
            save_credentials,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
