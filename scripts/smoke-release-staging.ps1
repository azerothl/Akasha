# Smoke-test a release staging folder (Windows). Usage: ./scripts/smoke-release-staging.ps1 staging
param(
    [string]$Staging = "staging"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

if (-not (Test-Path $Staging)) {
    Write-Host "::error::Staging directory not found: $Staging"
    exit 1
}

$Daemon = Join-Path $Staging "akasha-daemon.exe"
if (-not (Test-Path $Daemon)) {
    Write-Host "::error::No akasha-daemon.exe in $Staging"
    exit 1
}

foreach ($rel in @("docs\user_guide.md", "scripts")) {
    if (-not (Test-Path (Join-Path $Staging $rel))) {
        Write-Host "::error::Expected $Staging\$rel missing"
        exit 1
    }
}

$DataDir = Join-Path ([System.IO.Path]::GetTempPath()) ("akasha-smoke-" + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $DataDir -Force | Out-Null
$proc = $null

try {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $Port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $listener.Dispose()

    $env:AKASHA_DATA_DIR = $DataDir
    $env:AKASHA_PORT = "$Port"

    $proc = Start-Process -FilePath $Daemon -WorkingDirectory $Root -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput "$env:TEMP\akasha-smoke-out.txt" -RedirectStandardError "$env:TEMP\akasha-smoke-err.txt"

    $ok = $false
    for ($i = 0; $i -lt 60; $i++) {
        try {
            $r = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/" -UseBasicParsing -TimeoutSec 2
            if ($r.StatusCode -eq 200) { $ok = $true; break }
        } catch { }
        Start-Sleep -Milliseconds 500
    }
    if (-not $ok) {
        Write-Host "::error::Daemon did not respond on port $Port"
        exit 1
    }

    (Invoke-WebRequest -Uri "http://127.0.0.1:$Port/" -UseBasicParsing).Content | Out-Null
    (Invoke-WebRequest -Uri "http://127.0.0.1:$Port/api/status" -UseBasicParsing).Content | Out-Null
    $docs = (Invoke-WebRequest -Uri "http://127.0.0.1:$Port/api/docs" -UseBasicParsing).Content
    if ($docs -notmatch "content") {
        Write-Host "::error::GET /api/docs unexpected body"
        exit 1
    }

    Write-Host "Smoke OK: $Staging (port $Port)"
}
finally {
    if ($null -ne $proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -Recurse -Force $DataDir -ErrorAction SilentlyContinue
}
