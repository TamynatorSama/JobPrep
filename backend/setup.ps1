# InterPrep backend setup
# Run once from the backend/ directory: .\setup.ps1
#   -Voice   also install the optional voice stack (Piper + VibeVoice TTS +
#            faster-whisper STT). Auto-picks the CUDA torch build if an NVIDIA
#            GPU is present, otherwise the CPU build (slower TTS).
#            Piper is the fast default voice; VibeVoice ("vibe-rt") is opt-in in
#            Settings for a more humanlike voice + a multi-interviewer panel.

param([switch]$Voice)

$ErrorActionPreference = "Stop"

Write-Host "==> Checking for Python 3.11 / 3.12 ..."

# Find a compatible Python (3.11 or 3.12 — NOT 3.14, which lacks package wheels)
$python = $null
foreach ($cmd in @("py -3.12", "py -3.11", "python3.12", "python3.11")) {
    try {
        $ver = & $cmd.Split()[0] $cmd.Split()[1..99] "--version" 2>&1
        if ($ver -match "3\.(11|12)") { $python = $cmd; break }
    } catch {}
}

if (-not $python) {
    Write-Host @"

ERROR: Python 3.11 or 3.12 not found.
  - Python 3.14 (your current default) is too new; most AI/ML packages lack wheels for it.
  - Download Python 3.12 from https://www.python.org/downloads/
  - Or install via: winget install Python.Python.3.12

"@ -ForegroundColor Red
    exit 1
}

Write-Host "==> Using $python"

# Create virtual environment
Write-Host "==> Creating virtual environment (.venv) ..."
& $python.Split()[0] $python.Split()[1..99] -m venv .venv

# Activate and install
Write-Host "==> Installing requirements ..."
& .\.venv\Scripts\pip install --upgrade pip
& .\.venv\Scripts\pip install -r requirements.txt

# Install Playwright browser
Write-Host "==> Installing Playwright Chromium browser ..."
& .\.venv\Scripts\playwright install chromium

# ── Optional voice stack ─────────────────────────────────────────────────────
if ($Voice) {
    Write-Host ""
    Write-Host "==> Installing voice stack (this is large) ..."

    # Detect an NVIDIA GPU so we can install the matching torch build.
    $hasGpu = $false
    try { & nvidia-smi *> $null; if ($LASTEXITCODE -eq 0) { $hasGpu = $true } } catch {}

    if ($hasGpu) {
        Write-Host "    NVIDIA GPU detected -> installing CUDA (cu121) torch build." -ForegroundColor Green
        & .\.venv\Scripts\pip install torch --index-url https://download.pytorch.org/whl/cu121
    } else {
        Write-Host "    No NVIDIA GPU detected -> installing CPU torch build (TTS will be slower)." -ForegroundColor Yellow
        & .\.venv\Scripts\pip install torch
    }

    & .\.venv\Scripts\pip install -r requirements-voice.txt

    # ── VibeVoice (humanlike "vibe-rt" voice + panel) ────────────────────────
    # Installed SEPARATELY and best-effort: VibeVoice pins an older transformers
    # (==4.51.3) and declares a large demo/server dep tree (gradio, aiortc, av,
    # …) that sends pip into hours of backtracking against the rest of the env.
    # So install it with --no-deps and add only the libs its inference path
    # actually imports. The older transformers is safe here — in this venv only
    # the now-unused voxcpm/funasr wanted the newer one, so we drop them first.
    # If any of this fails, the default Piper voice is unaffected.
    Write-Host "==> Installing VibeVoice (humanlike voice; large, best-effort) ..."
    try {
        & .\.venv\Scripts\pip uninstall -y voxcpm funasr 2>$null
        & .\.venv\Scripts\pip install --no-deps "git+https://github.com/microsoft/VibeVoice.git"
        if ($LASTEXITCODE -ne 0) { throw "vibevoice wheel build failed" }
        # VibeVoice inference deps only (no gradio/aiortc/av/uvicorn/fastapi/pydub).
        & .\.venv\Scripts\pip install "transformers==4.51.3" accelerate diffusers `
            "numba>=0.57.0" "llvmlite>=0.40.0" scipy librosa ml-collections absl-py tqdm
        if ($LASTEXITCODE -ne 0) { throw "vibevoice deps failed" }
        Write-Host "    VibeVoice installed." -ForegroundColor Green
    } catch {
        Write-Host "    VibeVoice install failed (default Piper voice still works): $_" -ForegroundColor Yellow
    }

    # Pre-fetch the Piper voice (fast default) so the first interview has no
    # download stall. Best-effort: the backend also lazy-downloads on first use.
    $piperDir = Join-Path (Get-Location) "models\piper"
    $piperVoice = "en_US-amy-medium"
    $piperBase = "https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/amy/medium"
    Write-Host "==> Fetching Piper voice ($piperVoice) ..."
    try {
        New-Item -ItemType Directory -Force $piperDir | Out-Null
        foreach ($ext in @("onnx", "onnx.json")) {
            $dest = Join-Path $piperDir "$piperVoice.$ext"
            if (-not (Test-Path $dest)) {
                Invoke-WebRequest "$piperBase/$piperVoice.$ext" -OutFile $dest
            }
        }
    } catch {
        Write-Host "    Piper voice download failed (will retry lazily on first use): $_" -ForegroundColor Yellow
    }

    # Pre-fetch the VibeVoice speaker presets (panelist voices). These ship only
    # in the GitHub repo (not the HF weights), so the realtime model needs them
    # locally. Best-effort: the backend also lazy-downloads each on first use.
    $vibeDir = Join-Path (Get-Location) "models\vibevoice\voices"
    $vibeBase = "https://raw.githubusercontent.com/microsoft/VibeVoice/main/demo/voices/streaming_model"
    $vibeVoices = @(
        "en-Carter_man.pt", "en-Davis_man.pt", "en-Emma_woman.pt",
        "en-Frank_man.pt", "en-Grace_woman.pt", "en-Mike_man.pt"
    )
    Write-Host "==> Fetching VibeVoice speaker presets ..."
    try {
        New-Item -ItemType Directory -Force $vibeDir | Out-Null
        foreach ($v in $vibeVoices) {
            $dest = Join-Path $vibeDir $v
            if (-not (Test-Path $dest)) {
                Invoke-WebRequest "$vibeBase/$v" -OutFile $dest
            }
        }
    } catch {
        Write-Host "    VibeVoice preset download failed (will retry lazily on first use): $_" -ForegroundColor Yellow
    }
    Write-Host "    VibeVoice-Realtime-0.5B weights + faster-whisper 'base' download on first use." -ForegroundColor Cyan
} else {
    Write-Host ""
    Write-Host "==> Voice stack skipped. Enable spoken mock interviews with: .\setup.ps1 -Voice" -ForegroundColor DarkGray
}

Write-Host ""
Write-Host "==> Setup complete!" -ForegroundColor Green
Write-Host "    The Rust app will auto-use .venv\Scripts\python.exe if it is present."
Write-Host "    Or set: `$env:INTERPREP_BACKEND_DIR = '$(Get-Location)'"
