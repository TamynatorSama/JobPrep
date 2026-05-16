//! Persistent storage for job records.
//!
//! Jobs aren't secrets — they're application data — so they go to a plain
//! JSON file under `%LOCALAPPDATA%\InterPrep\jobs.json` rather than the
//! Windows Credential Manager. Failures (missing dir, bad JSON, denied
//! write) log to stderr and degrade to defaults; the app keeps running.
//!
//! The file is rewritten atomically: write to `jobs.json.tmp`, then rename
//! over the real file. That prevents a half-written file if the process
//! crashes mid-save.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::gui::types::Job;

const FILE_NAME: &str = "jobs.json";

/// Returns the directory where InterPrep stores per-user data.
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

/// Loads the persisted jobs list. Returns `None` if the file doesn't exist
/// (first run) or if it can't be parsed — the caller should fall back to
/// seed/sample data in that case.
pub fn load() -> Option<Vec<Job>> {
    let path = jobs_path();
    let bytes = match fs::read(&path) {
        Ok(b)  => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("jobs_store: read failed at {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_slice::<Vec<Job>>(&bytes) {
        Ok(mut jobs) => {
            for job in &mut jobs {
                if job.research_status.trim().is_empty() {
                    job.research_status = if job.job_description.trim().is_empty() {
                        "No JD added".to_owned()
                    } else {
                        "Not started".to_owned()
                    };
                }
            }
            Some(jobs)
        }
        Err(e) => {
            eprintln!("jobs_store: parse failed at {}: {e}", path.display());
            None
        }
    }
}

/// Persists the jobs list. Failures are logged but never panic.
pub fn save(jobs: &[Job]) {
    let dir = data_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("jobs_store: cannot create {}: {e}", dir.display());
        return;
    }
    let path = jobs_path();
    let tmp  = path.with_extension("json.tmp");

    let json = match serde_json::to_vec_pretty(jobs) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("jobs_store: serialize failed: {e}");
            return;
        }
    };

    // Write to tmp file, then atomic rename over the real one.
    match fs::File::create(&tmp).and_then(|mut f| f.write_all(&json).and(f.sync_all())) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("jobs_store: write to {} failed: {e}", tmp.display());
            return;
        }
    }
    if let Err(e) = replace_file(&tmp, &path) {
        eprintln!("jobs_store: replace {} -> {} failed: {e}", tmp.display(), path.display());
    }
}

fn replace_file(tmp: &std::path::Path, path: &std::path::Path) -> std::io::Result<()> {
    match fs::rename(tmp, path) {
        Ok(()) => return Ok(()),
        Err(e) if !path.exists() => return Err(e),
        Err(_) => {}
    }

    let backup = path.with_extension("json.bak");
    match fs::remove_file(&backup) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }

    fs::rename(path, &backup)?;
    if let Err(e) = fs::rename(tmp, path) {
        let _ = fs::rename(&backup, path);
        return Err(e);
    }
    match fs::remove_file(&backup) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(())
}
