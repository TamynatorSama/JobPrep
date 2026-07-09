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

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn log_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("InterPrep")
}

/// Load the persistent bridge token, minting one on first run. Persisted so
/// the extension pairing code (base64 of "port:token") survives app restarts —
/// a per-boot token forced the user to re-pair the extension every launch.
/// Same trust domain as the rest of `%LOCALAPPDATA%\InterPrep` (jobs.json,
/// bridge.json): local-file read access already implies bridge access while
/// the app runs, and the token is useless once the sidecar is down.
pub fn load_or_mint_token(mint: impl FnOnce() -> String) -> String {
    let path = log_dir().join("bridge_token");
    if let Ok(s) = fs::read_to_string(&path) {
        let t = s.trim().to_string();
        // 64 hex chars = the 256-bit token this app writes; anything else is
        // corrupt/foreign — mint fresh rather than trust it.
        if t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            return t;
        }
    }
    let t = mint();
    let _ = fs::create_dir_all(log_dir());
    let _ = fs::write(&path, &t);
    t
}

/// Write `%LOCALAPPDATA%\InterPrep\bridge.json` = `{port, token}` so the
/// browser extension can discover the (dynamic) port and the shared secret.
/// The Gemini key is never written here. Best-effort: a failure just means the
/// user pairs manually from the values shown in the app.
fn write_bridge_file(port: u16, token: &str) {
    let dir = log_dir();
    let _ = fs::create_dir_all(&dir);
    let body = format!("{{\n  \"port\": {port},\n  \"token\": \"{token}\"\n}}\n");
    let path = dir.join("bridge.json");
    let tmp = dir.join("bridge.json.tmp");
    if fs::write(&tmp, body.as_bytes()).is_ok() {
        let _ = fs::rename(&tmp, &path);
    }
}

pub struct PythonSidecar {
    /// Held only so the child process is killed when `Drop` runs.
    /// Stored in `Option<>` because we use `.take()` on shutdown.
    process: Option<Child>,
    port: u16,
}

impl PythonSidecar {
    /// `token` is the bridge shared secret; `gemini_key` seeds the in-memory
    /// Gemini key so the extension's autofill requests don't have to carry it.
    /// Both are passed to Python via env vars (never over HTTP / never to disk
    /// for the key).
    pub fn start(token: &str, gemini_key: &str) -> Result<Self, String> {
        // Always prepare the log file FIRST so diagnostic info survives even
        // when discovery/spawn fails. Truncate on every boot — old log noise
        // makes the current failure harder to spot.
        let log_path = log_dir().join("sidecar.log");
        let _ = fs::create_dir_all(log_dir());
        let mut diag = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&log_path)
            .ok();
        let mut log = |msg: &str| {
            if let Some(f) = diag.as_mut() {
                let _ = writeln!(f, "[sidecar] {msg}");
                let _ = f.flush();
            }
        };

        log(&format!(
            "cwd={:?}",
            std::env::current_dir().ok()
        ));
        log(&format!(
            "exe={:?}",
            std::env::current_exe().ok()
        ));

        let backend_dir = find_backend_dir().ok_or_else(|| {
            let msg = "Could not locate the backend directory. \
                Set INTERPREP_BACKEND_DIR or place a `backend/` folder next to the executable.";
            log(msg);
            msg.to_string()
        })?;
        log(&format!("backend_dir={}", backend_dir.display()));

        let port = pick_port();
        let (python_exe, python_prefix_args) = find_python().map_err(|e| {
            log(&format!("find_python failed: {e}"));
            e
        })?;
        log(&format!(
            "python_exe={python_exe} prefix_args={python_prefix_args:?} port={port}"
        ));

        let mut args: Vec<String> = python_prefix_args.clone();
        args.extend([
            // Unbuffered stdio so uvicorn/import errors land in sidecar.log in
            // real time. Without this, a crash during the ~18s cold import
            // walk surfaces as a silent timeout.
            "-u".into(),
            "-m".into(),
            "uvicorn".into(),
            "main:app".into(),
            "--host".into(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            // Info-level keeps uvicorn lifecycle + our own [chat]/[voice]
            // prints in sidecar.log, but NOT per-request access logs: the UI
            // polls /inbox/* every few seconds, which floods the log (tens of
            // thousands of lines/day) and buries real errors.
            "--log-level".into(),
            "info".into(),
            "--no-access-log".into(),
        ]);
        log(&format!("spawn args: {args:?}"));

        // Drop the diagnostic handle BEFORE re-opening for stdio so Windows
        // doesn't block uvicorn from writing (default File handle sharing
        // excludes write).
        drop(diag.take());

        // Pipe stdout + stderr to the same log file. Open once and clone so
        // both streams share a single handle with write sharing implicit.
        let stdio_handle = match fs::OpenOptions::new()
            .write(true)
            .append(true)
            .open(&log_path)
        {
            Ok(f) => f,
            Err(e) => {
                return Err(format!(
                    "could not open sidecar.log for writing ({}): {e}",
                    log_path.display()
                ));
            }
        };
        let stderr_handle = match stdio_handle.try_clone() {
            Ok(f) => f,
            Err(e) => {
                return Err(format!("could not clone stdio handle: {e}"));
            }
        };

        let child = Command::new(&python_exe)
            .args(&args)
            .current_dir(&backend_dir)
            // Bridge secret + Gemini key handed to Python via env (the key is
            // never written to disk; the token is also published to bridge.json
            // below so the extension can be paired).
            .env("INTERPREP_BRIDGE_TOKEN", token)
            .env("INTERPREP_GEMINI_KEY", gemini_key)
            .stdout(Stdio::from(stdio_handle))
            .stderr(Stdio::from(stderr_handle))
            .spawn()
            .map_err(|e| {
                let msg = format!("failed to spawn Python ({python_exe}): {e}");
                // Best-effort append the spawn error to the log too.
                if let Ok(mut f) = fs::OpenOptions::new()
                    .append(true)
                    .open(&log_path)
                {
                    let _ = writeln!(f, "[sidecar] {msg}");
                }
                msg
            })?;

        let mut sidecar = Self {
            process: Some(child),
            port,
        };
        sidecar.wait_ready().map_err(|e| {
            if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&log_path) {
                let _ = writeln!(f, "[sidecar] wait_ready failed: {e}");
            }
            e
        })?;
        // Publish {port, token} so the browser extension can be paired. The
        // Gemini key is deliberately NOT written here — secrets stay in memory.
        write_bridge_file(port, token);
        Ok(sidecar)
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        // Cold-start budget. Warm boot is ~9s, but a COLD first import (after a
        // reboot or a fresh dependency install) can hit ~45s — and once the
        // optional voice stack (torch/CUDA, ~GB of DLLs) is installed, Windows
        // Defender scanning those DLLs on first load can push it well past a
        // minute. 90s occasionally lost that race ("backend failed"); 180s
        // gives ample slack. Warm boots still return as soon as /health answers,
        // so this only affects the genuinely-slow first launch.
        const READY_TIMEOUT_SECS: u64 = 180;
        let url = format!("http://127.0.0.1:{}/health", self.port);
        let deadline = Instant::now() + Duration::from_secs(READY_TIMEOUT_SECS);
        while Instant::now() < deadline {
            // Fail fast if the process crashed during import (e.g. a bad
            // dependency) instead of waiting out the whole timeout. The real
            // traceback is in sidecar.log.
            if let Some(child) = self.process.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(format!(
                        "Python backend exited during startup ({status}). See sidecar.log for the traceback."
                    ));
                }
            }
            let ok = reqwest::blocking::get(&url)
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if ok {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        Err(format!(
            "Python backend did not become ready within {READY_TIMEOUT_SECS} seconds"
        ))
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl PythonSidecar {
    /// Kill the sidecar and every descendant (uvicorn workers and anything the
    /// Python process spawned). Idempotent — safe to call from both an explicit
    /// shutdown handler and `Drop`.
    ///
    /// On Windows, `Child::kill` only terminates the immediate process; any
    /// children the interpreter spawned become orphans unless we walk the
    /// tree. `taskkill /T /F /PID …` does exactly that.
    pub fn shutdown(&mut self) {
        if let Some(mut child) = self.process.take() {
            let pid = child.id();
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &pid.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            // Belt-and-braces: also call kill() in case taskkill failed (PID
            // already gone, missing on PATH on some restricted machines).
            let _ = child.kill();
            let _ = child.wait();
            let _ = pid;
        }
        // The bridge is only valid while the sidecar runs — remove the pairing
        // file so a stale port/token can't be used after shutdown.
        let _ = fs::remove_file(log_dir().join("bridge.json"));
    }
}

impl Drop for PythonSidecar {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ─── Discovery helpers ────────────────────────────────────────────────────

fn find_backend_dir() -> Option<PathBuf> {
    // 1. Explicit override.
    if let Ok(dir) = std::env::var("INTERPREP_BACKEND_DIR") {
        let p = PathBuf::from(dir);
        if p.join("main.py").exists() {
            return p.canonicalize().ok();
        }
    }

    // 2. Walk up from the executable. Covers both installed layouts and
    //    `cargo run` from a deep target dir like `C:\ip_tauri\debug\`.
    if let Ok(exe) = std::env::current_exe() {
        let mut cursor: Option<&std::path::Path> = exe.parent();
        for _ in 0..6 {
            let Some(dir) = cursor else { break; };
            let candidate = dir.join("backend").join("main.py");
            if candidate.exists() {
                return candidate.parent().and_then(|p| p.canonicalize().ok());
            }
            cursor = dir.parent();
        }
    }

    // 3. Walk up from the current working directory. `tauri dev` runs cargo
    //    from `src-tauri/`, so `./backend/` doesn't exist — but `../backend/`
    //    does. Repo roots can be a few levels up depending on the runner.
    if let Ok(cwd) = std::env::current_dir() {
        let mut cursor: Option<&std::path::Path> = Some(cwd.as_path());
        for _ in 0..6 {
            let Some(dir) = cursor else { break; };
            let candidate = dir.join("backend").join("main.py");
            if candidate.exists() {
                return candidate.parent().and_then(|p| p.canonicalize().ok());
            }
            cursor = dir.parent();
        }
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

/// Pick the sidecar port, preferring the one used last boot so the extension
/// pairing code (which embeds the port) keeps working across app restarts.
/// Falls back to a fresh OS-assigned port when the remembered one is taken.
/// The bind test releases the port before uvicorn claims it — the same benign
/// race `find_free_port` already has on a single-user localhost.
fn pick_port() -> u16 {
    use std::net::TcpListener;
    let path = log_dir().join("last_port");
    if let Ok(s) = fs::read_to_string(&path) {
        if let Ok(p) = s.trim().parse::<u16>() {
            if p != 0 && TcpListener::bind(("127.0.0.1", p)).is_ok() {
                return p;
            }
        }
    }
    let p = find_free_port().unwrap_or(8765);
    let _ = fs::create_dir_all(log_dir());
    let _ = fs::write(&path, p.to_string());
    p
}
