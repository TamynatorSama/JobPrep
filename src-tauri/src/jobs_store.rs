//! Persistent storage for job records.
//!
//! Jobs aren't secrets — they're application data — so they go to a plain
//! JSON file under `%LOCALAPPDATA%\InterPrep\jobs.json` rather than the
//! Windows Credential Manager.
//!
//! The file is rewritten atomically: write to `jobs.json.tmp`, then rename
//! over the real file. That prevents a half-written file if the process
//! crashes mid-save.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::types::Job;

const FILE_NAME: &str = "jobs.json";

fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("InterPrep")
}

fn jobs_path() -> PathBuf {
    data_dir().join(FILE_NAME)
}

/// Loads the persisted jobs list. Returns an empty vec on first run or if
/// the file can't be parsed — the frontend can detect "empty" and seed with
/// sample data if it wants.
pub fn load() -> Vec<Job> {
    let path = jobs_path();
    match fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Vec<Job>>(&bytes) {
            Ok(jobs) => jobs,
            Err(e) => {
                eprintln!("jobs_store: parse failed at {}: {e}", path.display());
                Vec::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!("jobs_store: read failed at {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Atomically persists the jobs list. Returns a human-readable error string
/// on failure so Tauri commands can pass it straight through to the frontend.
pub fn save(jobs: &[Job]) -> Result<(), String> {
    let dir = data_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create directory {}: {e}", dir.display()))?;

    let path = jobs_path();
    let tmp  = path.with_extension("json.tmp");

    let json = serde_json::to_vec_pretty(jobs)
        .map_err(|e| format!("serialize failed: {e}"))?;

    {
        let mut f = fs::File::create(&tmp)
            .map_err(|e| format!("create {} failed: {e}", tmp.display()))?;
        f.write_all(&json)
            .map_err(|e| format!("write {} failed: {e}", tmp.display()))?;
        f.sync_all()
            .map_err(|e| format!("sync {} failed: {e}", tmp.display()))?;
    }

    fs::rename(&tmp, &path)
        .map_err(|e| format!("rename {} -> {} failed: {e}", tmp.display(), path.display()))
}
