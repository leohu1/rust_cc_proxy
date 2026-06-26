# rust_cc_proxy launch script
# Usage: .\start-proxy.ps1 [-Provider deepseek|anthropic] [-Port 8787]
#        or configure via environment variables (see below)
param(
    [ValidateSet("deepseek", "anthropic")]
    [string]$Provider = "",
    [int]$Port = 0,
    [switch]$Discovery,
    [switch]$Compression
)

$ErrorActionPreference = "Stop"

# ── Help ──────────────────────────────────────────────────────────
if ($args -contains "-h" -or $args -contains "--help") {
    Write-Host @"
rust_cc_proxy launch script

Usage:
  .\start-proxy.ps1 [-Provider deepseek|anthropic] [-Port 8787] [-Discovery] [-Compression]

Parameters:
  -Provider     Backend provider: deepseek (default if DEEPSEEK_UPSTREAM is set) or anthropic
  -Port         Listen port (default: 8787 or `$env:PROXY_PORT)
  -Discovery    Print the Claude Code command for CC Switch model discovery
  -Compression  Enable token compression (future)

Environment variables (set before running, or use params):
  DEEPSEEK_UPSTREAM       DeepSeek API URL (default: https://api.deepseek.com/anthropic)
  DEEPSEEK_API_KEY        DeepSeek API key (required for DeepSeek)
  DEEPSEEK_DEFAULT_MODEL  Default DeepSeek model (default: deepseek-v4-pro)
  DEEPSEEK_MODEL_MAP      Model name mappings: client1=upstream1,client2=upstream2
  PROXY_UPSTREAM          Anthropic upstream URL (default: https://api.anthropic.com)
  PROXY_API_KEY           Anthropic API key
  PROXY_HOST              Bind address (default: 127.0.0.1)
  PROXY_PORT              Bind port (default: 8787)
  PROXY_LOG_LEVEL         Log level (default: info)
  PROXY_TIMEOUT           Request timeout in seconds (default: 600)

Examples:
  .\start-proxy.ps1 -Provider deepseek
  .\start-proxy.ps1 -Provider deepseek -Port 9090 -Discovery
  `$env:DEEPSEEK_API_KEY="sk-..."; .\start-proxy.ps1 -Provider deepseek
"@
    exit 0
}

# ── Detect provider ───────────────────────────────────────────────
# Auto-detection is built into the proxy: if DEEPSEEK_API_KEY (or DEEPSEEK_UPSTREAM)
# is set, DeepSeek becomes the default. Otherwise falls back to Anthropic passthrough.
# The -Provider param is only used for display purposes here.
if ($Provider -eq "") {
    if ($env:DEEPSEEK_UPSTREAM -or $env:DEEPSEEK_API_KEY) {
        $Provider = "deepseek"
    } else {
        $Provider = "anthropic"
    }
}

# ── Set defaults ──────────────────────────────────────────────────
# DEEPSEEK_UPSTREAM default is handled by the proxy code itself;
# only set it here when we need to display it.
if (-not $env:DEEPSEEK_DEFAULT_MODEL) {
    $env:DEEPSEEK_DEFAULT_MODEL = "deepseek-v4-pro"
}
if (-not $env:DEEPSEEK_MODEL_MAP) {
    # Default: map common Claude model names to DeepSeek
    $env:DEEPSEEK_MODEL_MAP = "sonnet=deepseek-v4-pro,haiku=deepseek-v4-flash,opus=deepseek-v4-pro"
}

if ($Port -ne 0) {
    $env:PROXY_PORT = [string]$Port
}
if (-not $env:PROXY_PORT) {
    $env:PROXY_PORT = "8787"
}
if (-not $env:PROXY_HOST) {
    $env:PROXY_HOST = "127.0.0.1"
}
if (-not $env:PROXY_LOG_LEVEL) {
    $env:PROXY_LOG_LEVEL = "info"
}

if ($Compression) {
    $env:COMPRESSION_ENABLED = "true"
}

# ── Validate ──────────────────────────────────────────────────────
if ($Provider -eq "deepseek" -and -not $env:DEEPSEEK_API_KEY) {
    Write-Host "ERROR: DEEPSEEK_API_KEY is not set." -ForegroundColor Red
    Write-Host "  Set it via environment variable: `$env:DEEPSEEK_API_KEY = 'sk-...'" -ForegroundColor Yellow
    Write-Host "  Then re-run: .\start-proxy.ps1 -Provider deepseek" -ForegroundColor Yellow
    exit 1
}
if ($Provider -eq "anthropic" -and -not $env:PROXY_API_KEY) {
    Write-Host "WARNING: PROXY_API_KEY is not set. Requests will rely on client-provided auth." -ForegroundColor Yellow
}

# ── Build ─────────────────────────────────────────────────────────
Write-Host "Building rust_cc_proxy..." -ForegroundColor Cyan
cargo build --release 2>&1 | Select-Object -Last 5
if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed. Run 'cargo build --release' manually for details." -ForegroundColor Red
    exit 1
}

# ── Display configuration ─────────────────────────────────────────
Write-Host ""
Write-Host "══════════════════════════════════════════════════════════" -ForegroundColor Green
Write-Host "  rust_cc_proxy v$(cargo pkgid 2>$null | Select-String -Pattern 'rust_cc_proxy@(.+?)$' | ForEach-Object { $_.Matches.Groups[1].Value })" -ForegroundColor Green
Write-Host "══════════════════════════════════════════════════════════" -ForegroundColor Green
Write-Host "  Provider   : $Provider" -ForegroundColor White
Write-Host "  Listen     : $env:PROXY_HOST`:$env:PROXY_PORT" -ForegroundColor White
Write-Host "  Log level  : $env:PROXY_LOG_LEVEL" -ForegroundColor White

if ($Provider -eq "deepseek") {
    $displayUrl = if ($env:DEEPSEEK_UPSTREAM.Length -gt 50) { $env:DEEPSEEK_UPSTREAM.Substring(0, 50) + "..." } else { $env:DEEPSEEK_UPSTREAM }
    Write-Host "  Upstream   : $displayUrl" -ForegroundColor White
    Write-Host "  Model      : $env:DEEPSEEK_DEFAULT_MODEL" -ForegroundColor White
    Write-Host "  Model map  : $env:DEEPSEEK_MODEL_MAP" -ForegroundColor White
    if ($env:DEEPSEEK_API_KEY) {
        $masked = $env:DEEPSEEK_API_KEY.Substring(0, [Math]::Min(10, $env:DEEPSEEK_API_KEY.Length)) + "..."
        Write-Host "  API key    : $masked" -ForegroundColor White
    }
} else {
    Write-Host "  Upstream   : $env:PROXY_UPSTREAM" -ForegroundColor White
}
if ($Compression) {
    Write-Host "  Compression: enabled" -ForegroundColor White
}
Write-Host "══════════════════════════════════════════════════════════" -ForegroundColor Green
Write-Host ""

# ── Start proxy ───────────────────────────────────────────────────
Write-Host "Starting proxy (Ctrl+C to stop)..." -ForegroundColor Cyan
Write-Host ""

cargo run --release

# ── CC Switch instructions ────────────────────────────────────────
if ($Discovery) {
    Write-Host ""
    Write-Host "══════════════════════════════════════════════════════════" -ForegroundColor Magenta
    Write-Host "  CC Switch — Claude Code launch command:" -ForegroundColor Magenta
    Write-Host "══════════════════════════════════════════════════════════" -ForegroundColor Magenta
    Write-Host ""
    Write-Host "  `$env:ANTHROPIC_BASE_URL          = 'http://$env:PROXY_HOST`:$env:PROXY_PORT'" -ForegroundColor Yellow
    Write-Host "  `$env:ANTHROPIC_API_KEY            = ''" -ForegroundColor Yellow
    Write-Host "  `$env:ANTHROPIC_AUTH_TOKEN         = 'any-value'" -ForegroundColor Yellow
    Write-Host "  `$env:CLAUDE_CODE_ATTRIBUTION_HEADER = '0'" -ForegroundColor Yellow
    Write-Host "  `$env:CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY = '1'" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "  Paste the above in a PowerShell terminal, then run: claude" -ForegroundColor White
    Write-Host "  In Claude Code, type /model to see available models." -ForegroundColor White
    Write-Host ""
}

# ── One-liner for copy-paste ──────────────────────────────────────
Write-Host "┌─ Copy-paste Claude Code launch (one line) ────────────────────────────────────────────────┐" -ForegroundColor DarkGray
Write-Host "| `$env:ANTHROPIC_BASE_URL='http://$env:PROXY_HOST`:$env:PROXY_PORT'; `$env:ANTHROPIC_API_KEY=''; `$env:ANTHROPIC_AUTH_TOKEN='any-value'; `$env:CLAUDE_CODE_ATTRIBUTION_HEADER='0'; `$env:CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY='1'; claude" -ForegroundColor DarkGray
Write-Host "└────────────────────────────────────────────────────────────────────────────────────────────┘" -ForegroundColor DarkGray
