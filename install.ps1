# rust_cc_proxy Windows Installer
# Usage: powershell -ExecutionPolicy Bypass -File install.ps1

param(
    [string]$InstallDir = "$env:USERPROFILE\.local\bin",
    [string]$ConfigDir = "$env:APPDATA\rust_cc_proxy",
    [string]$RepoUrl = "https://github.com/leohu1/rust_cc_proxy.git",
    [string]$Branch = "master",
    [switch]$SkipRustCheck,
    [switch]$DebugBuild
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

function Write-Step { Write-Host "`n:: $args`n" -ForegroundColor Cyan }
function Write-Info  { Write-Host "[INFO] $args" -ForegroundColor Green }
function Write-Warn  { Write-Host "[WARN] $args" -ForegroundColor Yellow }
function Write-Err   { Write-Host "[ERROR] $args" -ForegroundColor Red; exit 1 }

# ── Banner ──────────────────────────────────────────────────────────
Write-Host @"
╭─────────────────────────────────────────────╮
│  rust_cc_proxy — Claude Code Proxy Installer │
╰─────────────────────────────────────────────╯
"@ -ForegroundColor Green

Write-Info "OS: Windows / $(Get-CimInstance Win32_OperatingSystem | Select-Object -ExpandProperty Caption)"

# ── Check/install Rust ──────────────────────────────────────────────
if (-not $SkipRustCheck) {
    Write-Step "Checking Rust toolchain"
    if (Get-Command cargo -ErrorAction SilentlyContinue) {
        $ver = (rustc --version)
        Write-Info "$ver"
    } else {
        Write-Info "Installing Rust via rustup..."
        Invoke-WebRequest -Uri https://win.rustup.rs -OutFile "$env:TEMP\rustup-init.exe"
        & "$env:TEMP\rustup-init.exe" -y
        $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
    }
}

# ── Clone / update repo ─────────────────────────────────────────────
$RepoDir = "$env:USERPROFILE\.rust_cc_proxy_repo"
if (Test-Path "$RepoDir\.git") {
    Write-Step "Updating repository"
    Set-Location $RepoDir
    git fetch origin $Branch
    git checkout $Branch
    git pull origin $Branch
} else {
    Write-Step "Cloning repository"
    git clone --branch $Branch $RepoUrl $RepoDir
    Set-Location $RepoDir
}

# ── Build ───────────────────────────────────────────────────────────
Write-Step "Building rust_cc_proxy"
if ($DebugBuild) {
    cargo build
    $BinSrc = "target\debug\rust_cc_proxy.exe"
} else {
    cargo build --release
    $BinSrc = "target\release\rust_cc_proxy.exe"
}

Write-Step "Building headroom-ffi DLL"
try {
    cargo build -p headroom-ffi --release 2>$null
} catch {
    Write-Warn "headroom-ffi build skipped"
}

# ── Install ─────────────────────────────────────────────────────────
Write-Step "Installing to $InstallDir"
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
New-Item -ItemType Directory -Force -Path $ConfigDir | Out-Null

Copy-Item -Force $BinSrc "$InstallDir\rust_cc_proxy.exe"

# Copy headroom DLL if built
if (Test-Path "target\release\headroom_ffi.dll") {
    Copy-Item -Force "target\release\headroom_ffi.dll" "$InstallDir\headroom_core.dll"
    Write-Info "Installed headroom DLL"
}

# ── PATH check ──────────────────────────────────────────────────────
Write-Step "Environment check"
if ($env:PATH -notmatch [regex]::Escape($InstallDir)) {
    Write-Warn "Add to PATH: $InstallDir"
    $current = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($current -notmatch [regex]::Escape($InstallDir)) {
        Write-Host "  Run: [Environment]::SetEnvironmentVariable('PATH', `"`$env:PATH;$InstallDir`", 'User')"
    }
}

# ── Verify ──────────────────────────────────────────────────────────
Write-Step "Verifying installation"
try {
    & "$InstallDir\rust_cc_proxy.exe" --version
} catch {
    Write-Warn "Binary may need VC++ runtime: https://aka.ms/vs/17/release/vc_redist.x64.exe"
}

# ── Done ────────────────────────────────────────────────────────────
Write-Host @"

  ✓ Installation complete!

  Binary:   $InstallDir\rust_cc_proxy.exe
  Config:   $ConfigDir

  Quick start:
    `$env:DEEPSEEK_API_KEY = 'sk-...'
    rust_cc_proxy

  Dev mode:
    rust_cc_proxy --dev

  With compression:
    `$env:COMPRESSION_ENABLED = 'true'
    rust_cc_proxy
"@ -ForegroundColor Green
