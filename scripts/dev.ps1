# Akasha development script (Windows PowerShell)
# Usage: .\scripts\dev.ps1 [daemon|ui|all]

param(
    [string]$Target = "all"
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = (Get-Location).Path }
Set-Location $ProjectRoot

function Start-Daemon {
    Write-Host "Starting Akasha daemon..." -ForegroundColor Cyan
    $daemonPath = Join-Path $ProjectRoot "target\debug\akasha-daemon.exe"
    if (-not (Test-Path $daemonPath)) {
        Write-Host "Building daemon first..." -ForegroundColor Yellow
        cargo build -p akasha-daemon
    }
    Start-Process -FilePath $daemonPath -NoNewWindow
    Write-Host "Daemon started." -ForegroundColor Green
}

function Start-UI {
    Write-Host "Starting Akasha UI..." -ForegroundColor Cyan
    Set-Location (Join-Path $ProjectRoot "apps\akasha-ui")
    if (-not (Test-Path "node_modules")) {
        npm install
    }
    npm run tauri dev
}

switch ($Target.ToLower()) {
    "daemon" { Start-Daemon }
    "ui"     { Start-UI }
    "all"    {
        Start-Daemon
        Start-Sleep -Seconds 2
        Start-UI
    }
    default  { Write-Host "Usage: .\scripts\dev.ps1 [daemon|ui|all]" }
}
