# Akasha installation script (Windows)
# Release archives include playwright-runner\ beside the binaries; Node.js is only needed for the managed browser feature.
# Usage: .\install.ps1 [-InstallDir <path>] [-NoAutoStart]
# -InstallDir: where to copy binaries (default: C:\Akasha)
# -NoAutoStart: do not register daemon to start at user logon

param(
    [string]$InstallDir = "C:\Akasha",
    [switch]$NoAutoStart,
    [switch]$DownloadEmbedded,
    [switch]$SkipEmbeddedDownload
)

$ErrorActionPreference = "Stop"

if ($InstallDir -match '^-[a-zA-Z]' -and -not (Test-Path -LiteralPath $InstallDir -ErrorAction SilentlyContinue)) {
    Write-Error "Invalid installation directory '$InstallDir'. Use an absolute path with -InstallDir (e.g. -InstallDir C:\Akasha)."
    exit 1
}

# Script must be run from the directory containing the extracted release (akasha.exe, akasha-daemon.exe, etc.)
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ParentDir = Split-Path -Parent $ScriptDir
# If run from scripts/ subfolder (e.g. in zip: akasha-windows-x86_64/scripts/install.ps1), binaries are in parent
$BinDir = if (Test-Path (Join-Path $ScriptDir "akasha.exe")) { $ScriptDir } elseif (Test-Path (Join-Path $ParentDir "akasha.exe")) { $ParentDir } else { $ScriptDir }

foreach ($exe in @("akasha.exe", "akasha-daemon.exe", "akasha-tui.exe")) {
    $src = Join-Path $BinDir $exe
    if (-not (Test-Path $src)) {
        Write-Warning "Binary not found: $src. Run this script from the folder where you extracted the release."
        exit 1
    }
}

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Copy-Item (Join-Path $BinDir "akasha.exe") $InstallDir -Force
Copy-Item (Join-Path $BinDir "akasha-daemon.exe") $InstallDir -Force
Copy-Item (Join-Path $BinDir "akasha-tui.exe") $InstallDir -Force
# CUDA release zips ship cudart/cuBLAS next to the exes; keep them beside the install target.
foreach ($pat in @("cudart64_*.dll", "cublas64_*.dll", "cublasLt64_*.dll")) {
    Get-ChildItem -Path $BinDir -Filter $pat -ErrorAction SilentlyContinue | ForEach-Object {
        Copy-Item $_.FullName $InstallDir -Force
    }
}
# Full docs/ and playwright-runner/ beside the binaries (daemon resolves Playwright next to the exe)
foreach ($folder in @("docs", "playwright-runner", "spec")) {
    $src = Join-Path $BinDir $folder
    if (Test-Path $src) {
        Copy-Item -Path $src -Destination $InstallDir -Recurse -Force
    }
}
Write-Host "Binaries installed to $InstallDir"

# Initial setup (idempotent: safe to run again)
$akashaExe = Join-Path $InstallDir "akasha.exe"
& $akashaExe init --defaults
if ($LASTEXITCODE -ne 0) {
    Write-Warning "init --defaults returned $LASTEXITCODE (non-fatal; you can run 'akasha init' manually)"
}

$isCudaBuild = @(Get-ChildItem -Path $InstallDir -Filter "cudart64_*.dll" -ErrorAction SilentlyContinue).Count -gt 0
if ($isCudaBuild) {
    Write-Host ""
    Write-Host "Build NVIDIA CUDA detecte : telechargez le modele GGUF (~1 Go) avant le premier chat GPU." -ForegroundColor Cyan
    Write-Host "  Commande : & '$akashaExe' config models embedded-download" -ForegroundColor Cyan
} else {
    Write-Host ""
    Write-Host "Build CPU (Candle) : utilisable immediatement ; le premier appel peut etre lent (1-3 min)." -ForegroundColor Cyan
}

$doDownload = $false
if ($DownloadEmbedded) { $doDownload = $true }
elseif (-not $SkipEmbeddedDownload -and $isCudaBuild) {
    $r = Read-Host "Telecharger le modele embarque maintenant (~1 Go) ? [Y/n]"
    $doDownload = ($r -eq "" -or $r -match "^(y|yes)$")
}
if ($doDownload) {
    Write-Host "Telechargement du modele embarque..."
    & $akashaExe config models embedded-download
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "embedded-download returned $LASTEXITCODE — relancez manuellement ou via l'assistant UI."
    }
}

# Optional: add to user PATH
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir", "User")
    Write-Host "Added $InstallDir to user PATH. Restart the terminal for it to take effect."
}

# Daemon at logon (Task Scheduler, no admin required)
if (-not $NoAutoStart) {
    $taskName = "AkashaDaemon"
    $action = New-ScheduledTaskAction -Execute $akashaExe -Argument "start" -WorkingDirectory $InstallDir
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger -Settings $settings -Force | Out-Null
    Write-Host "Scheduled task '$taskName' created: daemon will start at your next logon. To start now: & '$akashaExe' start"
} else {
    Write-Host "Skipped auto-start. To start the daemon manually: & '$akashaExe' start"
}

# Start daemon once (background, so installer does not wait)
Write-Host "Starting daemon once..."
try {
    Start-Process -FilePath $akashaExe -ArgumentList "start" -WorkingDirectory $InstallDir -WindowStyle Hidden -ErrorAction Stop
    Write-Host "Daemon started."
} catch {
    Write-Warning "Could not start the daemon automatically: $($_.Exception.Message)"
    Write-Host "Start it manually: & '$akashaExe' start"
}

Write-Host "Installation complete."
