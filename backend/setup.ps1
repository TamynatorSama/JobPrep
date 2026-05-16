# InterPrep backend setup
# Run once from the backend/ directory: .\setup.ps1

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

Write-Host ""
Write-Host "==> Setup complete!" -ForegroundColor Green
Write-Host "    The Rust app will auto-use .venv\Scripts\python.exe if it is present."
Write-Host "    Or set: `$env:INTERPREP_BACKEND_DIR = '$(Get-Location)'"
