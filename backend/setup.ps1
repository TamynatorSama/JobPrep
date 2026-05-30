# InterPrep backend setup
# Run once from the backend/ directory: .\setup.ps1
#   -Voice   also install the optional voice stack (VoxCPM TTS + faster-whisper
#            STT). Auto-picks the CUDA torch build if an NVIDIA GPU is present,
#            otherwise the CPU build (slower TTS).

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
    Write-Host "    Voice models download on first use (VoxCPM-0.5B + faster-whisper 'base')." -ForegroundColor Cyan
} else {
    Write-Host ""
    Write-Host "==> Voice stack skipped. Enable spoken mock interviews with: .\setup.ps1 -Voice" -ForegroundColor DarkGray
}

Write-Host ""
Write-Host "==> Setup complete!" -ForegroundColor Green
Write-Host "    The Rust app will auto-use .venv\Scripts\python.exe if it is present."
Write-Host "    Or set: `$env:INTERPREP_BACKEND_DIR = '$(Get-Location)'"
