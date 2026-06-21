# Smoke-test a release staging folder (Windows). Usage: ./scripts/smoke-release-staging.ps1 staging
param(
    [string]$Staging = "staging"
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

function Show-SmokeDaemonLogs {
    param(
        [string]$OutFile,
        [string]$ErrFile
    )
    foreach ($entry in @(
            @{ Label = "stdout"; Path = $OutFile },
            @{ Label = "stderr"; Path = $ErrFile }
        )) {
        if (Test-Path $entry.Path) {
            Write-Host "=== akasha-daemon $($entry.Label) (tail) ==="
            Get-Content $entry.Path -Tail 80 -ErrorAction SilentlyContinue | ForEach-Object { Write-Host $_ }
        }
    }
}

if (-not (Test-Path $Staging)) {
    Write-Host "::error::Staging directory not found: $Staging"
    exit 1
}

$Daemon = Join-Path $Staging "akasha-daemon.exe"
if (-not (Test-Path $Daemon)) {
    Write-Host "::error::No akasha-daemon.exe in $Staging"
    exit 1
}

foreach ($rel in @("docs\user\index.json", "docs\user_guide.md", "scripts", "spec\tools_policy.example.yaml")) {
    if (-not (Test-Path (Join-Path $Staging $rel))) {
        Write-Host "::error::Expected $Staging\$rel missing"
        exit 1
    }
}

$DataDir = Join-Path ([System.IO.Path]::GetTempPath()) ("akasha-smoke-" + [guid]::NewGuid().ToString())
New-Item -ItemType Directory -Path $DataDir -Force | Out-Null
$proc = $null
$outFile = $null
$errFile = $null

try {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $Port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $listener.Dispose()

    $env:AKASHA_DATA_DIR = $DataDir
    $env:AKASHA_PORT = "$Port"
    # Skip Ollama LAN scan during CI smoke (daemon startup stays local-only).
    $env:OLLAMA_HOST = "http://127.0.0.1:11434"

    $stagingAbs = (Resolve-Path (Join-Path $Root $Staging)).Path
    if ($env:CUDA_PATH) {
        $cudaBin = Join-Path $env:CUDA_PATH "bin"
        if (Test-Path $cudaBin) {
            $env:PATH = "$cudaBin;$env:PATH"
        }
    }
    $env:PATH = "$stagingAbs;$env:PATH"

    $outFile = Join-Path $env:TEMP "akasha-smoke-out-$Port.txt"
    $errFile = Join-Path $env:TEMP "akasha-smoke-err-$Port.txt"
    Remove-Item $outFile, $errFile -ErrorAction SilentlyContinue

    $proc = Start-Process -FilePath $Daemon -WorkingDirectory $stagingAbs -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $outFile -RedirectStandardError $errFile

    $ok = $false
    for ($i = 0; $i -lt 120; $i++) {
        if ($proc.HasExited) {
            Write-Host "::error::Daemon exited before listening (code $($proc.ExitCode))"
            Show-SmokeDaemonLogs -OutFile $outFile -ErrFile $errFile
            exit 1
        }
        try {
            $r = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/" -UseBasicParsing -TimeoutSec 2
            if ($r.StatusCode -eq 200) { $ok = $true; break }
        } catch { }
        Start-Sleep -Milliseconds 500
    }
    if (-not $ok) {
        Write-Host "::error::Daemon did not respond on port $Port"
        Show-SmokeDaemonLogs -OutFile $outFile -ErrFile $errFile
        exit 1
    }

    (Invoke-WebRequest -Uri "http://127.0.0.1:$Port/" -UseBasicParsing).Content | Out-Null
    (Invoke-WebRequest -Uri "http://127.0.0.1:$Port/api/status" -UseBasicParsing).Content | Out-Null
    $docs = (Invoke-WebRequest -Uri "http://127.0.0.1:$Port/api/docs" -UseBasicParsing).Content
    if ($docs -notmatch "pages") {
        Write-Host "::error::GET /api/docs index unexpected body"
        exit 1
    }
    $page = (Invoke-WebRequest -Uri "http://127.0.0.1:$Port/api/docs/accueil" -UseBasicParsing).Content
    if ($page -notmatch "content") {
        Write-Host "::error::GET /api/docs/accueil unexpected body"
        exit 1
    }

    Write-Host "Smoke OK: $Staging (port $Port)"
}
finally {
    if ($null -ne $proc -and -not $proc.HasExited) {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -Recurse -Force $DataDir -ErrorAction SilentlyContinue
    Remove-Item $outFile, $errFile -ErrorAction SilentlyContinue
}
