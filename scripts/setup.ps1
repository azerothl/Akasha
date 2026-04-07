# Akasha unified setup (Windows)
# Run after extracting the "full" zip. Prompts for: install desktop app (Tauri), daemon at logon.
# The release zip also includes playwright-runner\ next to the binaries (managed browser). Node.js + npm are required only if you use that feature; the daemon can auto-install deps on first use unless AKASHA_PLAYWRIGHT_AUTO_INSTALL=0.
# Usage: .\setup.ps1 [-InstallDir <path>] [-InstallUi] [-NoInstallUi] [-AutoStart] [-NoAutoStart]
# Without flags, prompts interactively.

param(
    [string]$InstallDir = "C:\Akasha",
    [switch]$InstallUi,
    [switch]$NoInstallUi,
    [switch]$AutoStart,
    [switch]$NoAutoStart
)

$ErrorActionPreference = "Stop"

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$ParentDir = Split-Path -Parent $ScriptDir
# When run from full zip: binaries may be in same dir as scripts (ParentDir) or in ScriptDir
$RootDir = if (Test-Path (Join-Path $ParentDir "akasha.exe")) { $ParentDir } elseif (Test-Path (Join-Path $ScriptDir "akasha.exe")) { $ScriptDir } else { $ParentDir }

$InstallScript = Join-Path $ScriptDir "install.ps1"
if (-not (Test-Path $InstallScript)) {
    $InstallScript = Join-Path $RootDir "scripts\install.ps1"
}
if (-not (Test-Path $InstallScript)) {
    Write-Host "install.ps1 not found. Run this script from the extracted Akasha full package." -ForegroundColor Red
    exit 1
}

Write-Host "=== Akasha installation ===" -ForegroundColor Cyan
Write-Host "This will install: CLI, daemon, TUI. Optionally: desktop app (interface web) and daemon at Windows logon."
Write-Host ""

# Resolve InstallUi / NoInstallUi
$doInstallUi = $null
if ($InstallUi) { $doInstallUi = $true }
elseif ($NoInstallUi) { $doInstallUi = $false }
if ($null -eq $doInstallUi) {
    $uiBundle = $false
    foreach ($base in @($RootDir, (Join-Path $RootDir "ui"))) {
        if (Get-ChildItem -Path $base -Filter "*.msi" -ErrorAction SilentlyContinue | Select-Object -First 1) { $uiBundle = $true; break }
        if (Get-ChildItem -Path $base -Filter "*-setup.exe" -ErrorAction SilentlyContinue | Select-Object -First 1) { $uiBundle = $true; break }
        if (Get-ChildItem -Path (Join-Path $base "msi") -Filter "*.msi" -ErrorAction SilentlyContinue | Select-Object -First 1) { $uiBundle = $true; break }
        if (Get-ChildItem -Path (Join-Path $base "nsis") -Filter "*.exe" -ErrorAction SilentlyContinue | Select-Object -First 1) { $uiBundle = $true; break }
    }
    if ($uiBundle) {
        $r = Read-Host "Install desktop app (web interface)? [Y/n]"
        $doInstallUi = ($r -eq "" -or $r -match "^(y|yes)$")
    } else {
        $doInstallUi = $false
    }
}

# Resolve AutoStart / NoAutoStart
$doAutoStart = $null
if ($AutoStart) { $doAutoStart = $true }
elseif ($NoAutoStart) { $doAutoStart = $false }
if ($null -eq $doAutoStart) {
    $r = Read-Host "Start Akasha daemon at every logon? [Y/n]"
    $doAutoStart = ($r -eq "" -or $r -match "^(y|yes)$")
}

# 1) Install CLI + daemon + TUI (init, PATH, optional task)
$installArgs = @("-InstallDir", $InstallDir)
if (-not $doAutoStart) { $installArgs += "-NoAutoStart" }
& $InstallScript @installArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# 2) Optional: install Tauri desktop app
if ($doInstallUi) {
    $msi = Get-ChildItem -Path $RootDir -Recurse -Filter "*.msi" -ErrorAction SilentlyContinue | Select-Object -First 1
    $nsis = Get-ChildItem -Path $RootDir -Recurse -Filter "*-setup.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($msi) {
        Write-Host "Installing desktop app (MSI)..."
        Start-Process -FilePath "msiexec.exe" -ArgumentList "/i", "`"$($msi.FullName)`"", "/quiet" -Wait -NoNewWindow
        Write-Host "Desktop app installed."
    } elseif ($nsis) {
        Write-Host "Installing desktop app (setup)..."
        Start-Process -FilePath $nsis.FullName -ArgumentList "/S" -Wait -NoNewWindow
        Write-Host "Desktop app installed."
    } else {
        Write-Host "Desktop app bundle not found in this package. Install it separately from the akasha-ui release." -ForegroundColor Yellow
    }
}

# (Daemon was already started once by install.ps1.)

Write-Host ""
Write-Host "Setup complete." -ForegroundColor Green
Write-Host "  CLI: $InstallDir\akasha.exe (add to PATH if needed)"
Write-Host "  Daemon: already started; will restart at logon if you chose auto-start."
Write-Host "  Desktop app: use Start menu or run 'Akasha' if installed."
$r = Read-Host "Lancer l'assistant de configuration maintenant ? [Y/n]"
if ($r -eq "" -or $r -match "^(y|yes)$") {
    $akashaExe = Join-Path $InstallDir "akasha.exe"
    & $akashaExe init
    Write-Host "You can also run: & '$akashaExe' tui   (terminal UI)"
}
