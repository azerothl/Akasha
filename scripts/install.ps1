# Akasha installation script (Windows)
# Usage: .\install.ps1 [-InstallDir <path>] [-NoAutoStart]
# -InstallDir: where to copy binaries (default: C:\Akasha)
# -NoAutoStart: do not register daemon to start at user logon

param(
    [string]$InstallDir = "C:\Akasha",
    [switch]$NoAutoStart
)

$ErrorActionPreference = "Stop"

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
if (Test-Path (Join-Path $BinDir "docs\user_guide.md")) {
    New-Item -ItemType Directory -Force -Path (Join-Path $InstallDir "docs") | Out-Null
    Copy-Item (Join-Path $BinDir "docs\user_guide.md") (Join-Path $InstallDir "docs") -Force
}
Write-Host "Binaries installed to $InstallDir"

# Initial setup (idempotent: safe to run again)
$akashaExe = Join-Path $InstallDir "akasha.exe"
& $akashaExe init --defaults
if ($LASTEXITCODE -ne 0) {
    Write-Warning "init --defaults returned $LASTEXITCODE (non-fatal; you can run 'akasha init' manually)"
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

Write-Host "Installation complete."
