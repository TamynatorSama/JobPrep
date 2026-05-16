use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct PythonSidecar {
    process: Child,
    pub port: u16,
}

impl PythonSidecar {
    pub fn start() -> anyhow::Result<Self> {
        let backend_dir = find_backend_dir().ok_or_else(|| {
            anyhow::anyhow!(
                "Could not find the backend directory. \
                 Set INTERPREP_BACKEND_DIR env var or place 'backend/' next to the executable."
            )
        })?;

        let port = find_free_port().unwrap_or(8765);
        let (python_exe, python_prefix_args) = find_python()?;

        let uvicorn_args: Vec<String> = python_prefix_args
            .iter()
            .map(|s| s.as_str())
            .chain(["-m", "uvicorn", "main:app", "--port"])
            .map(str::to_owned)
            .chain([port.to_string()])
            .chain(["--host".to_owned(), "127.0.0.1".to_owned(),
                    "--log-level".to_owned(), "error".to_owned()])
            .collect();

        let child = Command::new(&python_exe)
            .args(&uvicorn_args)
            .current_dir(&backend_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn Python process: {e}"))?;

        let sidecar = Self { process: child, port };
        sidecar.wait_ready()?;
        Ok(sidecar)
    }

    fn wait_ready(&self) -> anyhow::Result<()> {
        let url = format!("http://127.0.0.1:{}/health", self.port);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if reqwest::blocking::get(&url)
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        anyhow::bail!("Python backend did not become ready within 30 seconds")
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for PythonSidecar {
    fn drop(&mut self) {
        let _ = self.process.kill();
    }
}

fn find_backend_dir() -> Option<PathBuf> {
    // 1. Explicit env var override
    if let Ok(dir) = std::env::var("INTERPREP_BACKEND_DIR") {
        let p = PathBuf::from(dir);
        if p.join("main.py").exists() {
            return Some(p);
        }
    }

    // 2. Relative to the executable (for installed/production layout)
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

    // 3. Working directory fallback (for `cargo run`)
    let p = PathBuf::from("backend");
    if p.join("main.py").exists() {
        return p.canonicalize().ok();
    }

    None
}

/// Returns `(executable, prefix_args)` so callers can do
/// `Command::new(exe).args(prefix_args).args(real_args)`.
/// e.g. `("py", ["-3.12"])` or `(".venv/Scripts/python.exe", [])`.
fn find_python() -> anyhow::Result<(String, Vec<String>)> {
    // 1. Virtualenv inside the backend directory (created by setup.ps1)
    if let Some(backend) = find_backend_dir() {
        let venv = backend.join(".venv").join("Scripts").join("python.exe");
        if venv.exists() {
            return Ok((venv.to_string_lossy().into_owned(), vec![]));
        }
    }

    // 2. Bundled portable Python next to the executable
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

    // 3. System Python — prefer 3.12 / 3.11 to avoid Python 3.14 missing wheels
    let candidates: &[(&str, &[&str])] = &[
        ("py",        &["-3.12"]),
        ("py",        &["-3.11"]),
        ("python3.12",&[]),
        ("python3.11",&[]),
        ("python3",   &[]),
        ("python",    &[]),
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
            return Ok((exe.to_string(), prefix.iter().map(|s| s.to_string()).collect()));
        }
    }

    anyhow::bail!(
        "Python 3.11 or 3.12 not found.\n\
         Run backend/setup.ps1 to create a .venv with the correct version,\n\
         or install Python 3.12 from https://www.python.org/downloads/.\n\
         (Python 3.14 on your system lacks wheels for LangGraph and Playwright.)"
    )
}

fn find_free_port() -> Option<u16> {
    use std::net::TcpListener;
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}
