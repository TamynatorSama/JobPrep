//! On-disk storage for the resume library and generated application files.
//!
//! Layout under `%APPDATA%/InterPrep/`:
//!   resumes.json                       — index + full text of each master resume
//!   applications/<job_id>/resume.docx  — generated tailored resume per job

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const APP_DIR_NAME: &str = "InterPrep";
const RESUME_INDEX_FILE: &str = "resumes.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resume {
    pub id:   u64,
    pub name: String,
    pub text: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ResumeIndex {
    next_id: u64,
    items:   Vec<Resume>,
}

#[derive(Debug)]
pub struct ResumeStore {
    items:   Vec<Resume>,
    next_id: u64,
}

impl ResumeStore {
    /// Load the resume library from disk. Returns an empty store if the
    /// index file doesn't exist or is unreadable — the user simply hasn't
    /// added any resumes yet.
    pub fn load() -> Self {
        let path = match index_path() {
            Some(p) => p,
            None => return Self::empty(),
        };
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Self::empty(),
        };
        match serde_json::from_slice::<ResumeIndex>(&bytes) {
            Ok(idx) => Self { items: idx.items, next_id: idx.next_id.max(1) },
            Err(_)  => Self::empty(),
        }
    }

    fn empty() -> Self { Self { items: Vec::new(), next_id: 1 } }

    pub fn items(&self) -> &[Resume] { &self.items }
    pub fn is_empty(&self) -> bool { self.items.is_empty() }
    pub fn len(&self) -> usize { self.items.len() }

    /// Insert a new resume; returns its assigned id. Persists to disk.
    pub fn add(&mut self, name: String, text: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Resume { id, name, text });
        let _ = self.save();
        id
    }

    /// Remove a resume by id. Persists to disk.
    pub fn remove(&mut self, id: u64) {
        self.items.retain(|r| r.id != id);
        let _ = self.save();
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = index_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no AppData directory available",
            ));
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let idx = ResumeIndex { next_id: self.next_id, items: self.items.clone() };
        let bytes = serde_json::to_vec_pretty(&idx).map_err(io_other)?;
        write_file_atomic(&path, &bytes)?;
        Ok(())
    }
}

/// Where the tailored docx for a given job should live.
/// Creates the directory if missing.
pub fn application_dir(job_id: usize) -> Option<PathBuf> {
    let dir = app_dir()?.join("applications").join(job_id.to_string());
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Save the .docx bytes for a job and return the file path.
pub fn save_resume_docx(job_id: usize, bytes: &[u8]) -> Option<PathBuf> {
    let dir  = application_dir(job_id)?;
    let path = dir.join("resume.docx");
    fs::write(&path, bytes).ok()?;
    Some(path)
}

/// Deterministic path for the company research markdown report for a job.
pub fn company_research_report_path(job_id: usize) -> Option<PathBuf> {
    application_dir(job_id).map(|dir| dir.join("company_research.md"))
}

/// Persist the completed company research report as markdown and return its path.
pub fn save_company_research_report(job_id: usize, markdown: &str) -> Option<PathBuf> {
    let path = company_research_report_path(job_id)?;
    write_file_atomic(&path, markdown.as_bytes()).ok()?;
    Some(path)
}

/// Load a previously-generated company research report, if one exists.
pub fn load_company_research_report(job_id: usize) -> Option<String> {
    let path = app_dir()?
        .join("applications")
        .join(job_id.to_string())
        .join("company_research.md");
    let text = fs::read_to_string(path).ok()?;
    if text.trim().is_empty() { None } else { Some(text) }
}

fn app_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(APP_DIR_NAME))
}

fn index_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join(RESUME_INDEX_FILE))
}

fn io_other<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

// ─── Resume file ingestion ───────────────────────────────────────────────────

/// Read a resume file and return `(display_name, plain_text)`.
/// Supports `.txt`, `.md`, `.docx`, and `.pdf`. Returns `None` if the format
/// is unrecognised or the file can't be parsed.
pub fn read_resume_file(path: &std::path::Path) -> Option<(String, String)> {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Resume")
        .to_owned();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    let text = match ext.as_str() {
        "txt" | "md" => fs::read_to_string(path).ok()?,
        "docx"       => extract_docx_text(path)?,
        "pdf"        => extract_pdf_text(path)?,
        _            => return None,
    };

    let trimmed = text.trim();
    if trimmed.is_empty() { None } else { Some((name, trimmed.to_owned())) }
}

/// Pure-Rust PDF text extraction via `pdf-extract`. Loses formatting but
/// preserves reading order, which is all we need — the LLM gets plain text.
fn extract_pdf_text(path: &std::path::Path) -> Option<String> {
    pdf_extract::extract_text(path).ok().filter(|t| !t.trim().is_empty())
}

/// Pull plain text out of a .docx by reading `word/document.xml` and stripping
/// XML tags. Preserves paragraph breaks; loses fancy formatting (which is fine
/// — we re-emit a clean ATS resume on the way out anyway).
fn extract_docx_text(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let file = fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut entry = zip.by_name("word/document.xml").ok()?;
    let mut xml = String::new();
    entry.read_to_string(&mut xml).ok()?;
    Some(strip_docx_xml(&xml))
}

fn strip_docx_xml(xml: &str) -> String {
    // Each <w:p> is a paragraph; inside, every <w:t>…</w:t> holds visible text.
    let mut out = String::new();
    for para in xml.split("</w:p>") {
        let mut line = String::new();
        for chunk in para.split("<w:t") {
            // Skip the part before the first <w:t (no opening tag yet)
            if let Some(gt) = chunk.find('>') {
                let after = &chunk[gt + 1..];
                if let Some(close) = after.find("</w:t>") {
                    line.push_str(&after[..close]);
                }
            }
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            out.push_str(trimmed);
            out.push('\n');
        }
    }
    decode_xml_entities(&out)
}

fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;",  "<")
        .replace("&gt;",  ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;",  "\n")
        .replace("&#9;",   "\t")
}

fn write_file_atomic(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    replace_file(&tmp, path)
}

fn replace_file(tmp: &std::path::Path, path: &std::path::Path) -> std::io::Result<()> {
    match fs::rename(tmp, path) {
        Ok(()) => return Ok(()),
        Err(e) if !path.exists() => return Err(e),
        Err(_) => {}
    }

    let backup = path.with_extension("bak");
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
