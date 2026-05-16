//! Python FastAPI sidecar — launches the backend process and keeps a handle
//! so it gets killed when the Tauri app exits.
//!
//! Discovery order for the backend directory:
//!   1. `INTERPREP_BACKEND_DIR` env var (explicit override)
//!   2. `backend/` next to the executable (installed layout)
//!   3. `backend/` in the current working directory (cargo run / tauri dev)
//!
//! Python is resolved by preferring a virtualenv (`backend/.venv`) before any
//! system Python, so the user's pinned dependency set is what runs.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct PythonSidecar {
    /// Held only so the child process is killed when `Drop` runs.
    /// Stored in `Option<>` because we use `.take()` on shutdown.
    process: Option<Child>,
    port: u16,
}

impl PythonSidecar {
    pub fn start() -> Result<Self, String> {
        let backend_dir = find_backend_dir().ok_or_else(|| {
            "Could not locate the backend directory. \
             Set INTERPREP_BACKEND_DIR or place a `backend/` folder next to the executable."
                .to_string()
        })?;

        let port = find_free_port().unwrap_or(8765);
        let (python_exe, python_prefix_args) = find_python()?;

        let mut args: Vec<String> = python_prefix_args.clone();
        args.extend([
            "-m".into(),
            "uvicorn".into(),
            "main:app".into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "--log-level".into(),
            "error".into(),
        ]);

        let child = Command::new(&python_exe)
            .args(&args)
            .current_dir(&backend_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn Python ({python_exe}): {e}"))?;

        let sidecar = Self {
            process: Some(child),
            port,
        };
        sidecar.wait_ready()?;
        Ok(sidecar)
    }

    fn wait_ready(&self) -> Result<(), String> {
        let url = format!("http://127.0.0.1:{}/health", self.port);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let ok = reqwest::blocking::get(&url)
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if ok {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Err("Python backend did not become ready within 30 seconds".into())
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for PythonSidecar {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
        }
    }
}

// ─── Discovery helpers ────────────────────────────────────────────────────

fn find_backend_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("INTERPREP_BACKEND_DIR") {
        let p = PathBuf::from(dir);
        if p.join("main.py").exists() {
            return Some(p);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for rel in &["backend", "../backend", "../../backend"] {
                let p = exe_dir.join(rel);
                if p.join("main.py").exists() {
                    return p.canonicalize().ok();
                }
            }
        }
    }

    let p = PathBuf::from("backend");
    if p.join("main.py").exists() {
        return p.canonicalize().ok();
    }
    None
}

/// Returns `(executable, prefix_args)`. Callers do
/// `Command::new(exe).args(prefix_args).args(real_args)`.
fn find_python() -> Result<(String, Vec<String>), String> {
    if let Some(backend) = find_backend_dir() {
        let venv = backend.join(".venv").join("Scripts").join("python.exe");
        if venv.exists() {
            return Ok((venv.to_string_lossy().into_owned(), vec![]));
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in &["python/python.exe", "python_env/Scripts/python.exe"] {
                let p = dir.join(candidate);
                if p.exists() {
                    return Ok((p.to_string_lossy().into_owned(), vec![]));
                }
            }
        }
    }

    let candidates: &[(&str, &[&str])] = &[
        ("py",         &["-3.12"]),
        ("py",         &["-3.11"]),
        ("python3.12", &[]),
        ("python3.11", &[]),
        ("python3",    &[]),
        ("python",     &[]),
    ];
    for (exe, prefix) in candidates {
        if Command::new(exe)
            .args(*prefix)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
        {
            return Ok((
                exe.to_string(),
                prefix.iter().map(|s| s.to_string()).collect(),
            ));
        }
    }

    Err(
        "Python 3.11 or 3.12 not found. Install Python from python.org or run \
         backend/setup.ps1 to create a `.venv`."
            .into(),
    )
}

fn find_free_port() -> Option<u16> {
    use std::net::TcpListener;
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}
