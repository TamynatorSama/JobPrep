//! Resume library — master resumes the user pastes once and then attaches
//! to jobs. Stored as JSON next to the jobs file under
//! `%LOCALAPPDATA%\InterPrep\resumes.json`.
//!
//! Same atomic-rename write strategy as `jobs_store` so a crash mid-save
//! never corrupts the file.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "resumes.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Resume {
    /// Stable per-resume id minted by the frontend (`Date.now()`). Used by
    /// jobs to reference which master resume they were created from.
    pub id:   u64,
    pub name: String,
    pub text: String,
    /// Base64-encoded raw .docx bytes when the resume was uploaded as a
    /// .docx file. Used by the backend as the styling base for in-place
    /// section editing so the tailored output preserves the original
    /// document's fonts, headings, and layout. `None` for PDF/MD/TXT
    /// uploads or for resumes saved before this field existed.
    #[serde(default)]
    pub docx_b64: Option<String>,
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("InterPrep")
}

fn resumes_path() -> PathBuf {
    data_dir().join(FILE_NAME)
}

pub fn load() -> Vec<Resume> {
    let path = resumes_path();
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Vec<Resume>>(&bytes).unwrap_or_else(|e| {
            eprintln!("resume_store: parse failed at {}: {e}", path.display());
            Vec::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            eprintln!("resume_store: read failed at {}: {e}", path.display());
            Vec::new()
        }
    }
}

pub fn save(resumes: &[Resume]) -> Result<(), String> {
    let dir = data_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let path = resumes_path();
    let tmp  = path.with_extension("json.tmp");

    let json = serde_json::to_vec_pretty(resumes)
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
        .map_err(|e| format!("rename failed: {e}"))
}
