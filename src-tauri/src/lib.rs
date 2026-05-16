//! InterPrep — Tauri runtime entry point.

mod jobs_store;
mod types;

use types::Job;

// ─── Commands ─────────────────────────────────────────────────────────────

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

// ─── Entry ────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![list_jobs, save_jobs])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
