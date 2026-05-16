//! InterPrep — Tauri runtime entry point.
//!
//! At this stage the backend exposes no commands. The frontend renders with
//! mock data so we can lock in the visual shell pixel-perfectly before
//! plumbing the real backend modules (jobs store, credentials, recorder,
//! Python sidecar, SSE chat stream) through Tauri IPC in the next phase.

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
