# Akasha build script (Windows PowerShell)
# Builds all crates and optionally the Tauri UI

param(
    [switch]$Release,
    [switch]$UI
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = (Get-Location).Path }
Set-Location $ProjectRoot

$ProfileArg = if ($Release) { "--release" } else { "" }

Write-Host "Building Akasha crates..." -ForegroundColor Cyan
cargo build $ProfileArg

if ($UI) {
    Write-Host "Building Akasha UI..." -ForegroundColor Cyan
    Set-Location (Join-Path $ProjectRoot "apps\akasha-ui")
    if (-not (Test-Path "node_modules")) {
        npm install
    }
    npm run tauri build
}

Write-Host "Build complete." -ForegroundColor Green
