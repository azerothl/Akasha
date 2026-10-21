param(
    [Parameter(Mandatory = $true)]
    [string]$ArtifactZip,
    [string]$ExpectedVersion = "",
    [int]$InferenceTimeoutSecs = 360,
    [string]$EvidenceDir = ""
)

$ErrorActionPreference = "Stop"

if ($InferenceTimeoutSecs -lt 1) {
    throw "InferenceTimeoutSecs must be positive"
}

$ArtifactPath = (Resolve-Path -LiteralPath $ArtifactZip).Path
$ExpectedVersion = $ExpectedVersion.TrimStart("v")
$Work = Join-Path $env:TEMP ("akasha-release-firstboot-" + [guid]::NewGuid().ToString())
$Extract = Join-Path $Work "extract"
$DataDir = Join-Path $Work "data"
$Log = Join-Path $Work "smoke.log"
$DoctorLog = Join-Path $Work "doctor.log"
$TaskLog = Join-Path $Work "task.json"
$EmbeddedLog = Join-Path $Work "embedded-status.json"
$DaemonOut = Join-Path $Work "daemon.stdout.log"
$DaemonErr = Join-Path $Work "daemon.stderr.log"
$Daemon = $null

New-Item -ItemType Directory -Path $Work, $Extract, $DataDir -Force | Out-Null

function Write-SmokeLog {
    param([string]$Message)
    $line = "[smoke-release-artifact] $Message"
    Write-Host $line
    Add-Content -LiteralPath $Log -Value $line -Encoding utf8
}

function Copy-Evidence {
    if ([string]::IsNullOrWhiteSpace($EvidenceDir)) {
        return
    }
    New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null
    foreach ($path in @($Log, $DoctorLog, $TaskLog, $EmbeddedLog, $DaemonOut, $DaemonErr)) {
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            Copy-Item -LiteralPath $path -Destination $EvidenceDir -Force
        }
    }
}

try {
    Expand-Archive -LiteralPath $ArtifactPath -DestinationPath $Extract -Force
    $Cli = Join-Path $Extract "akasha.exe"
    if (-not (Test-Path -LiteralPath $Cli -PathType Leaf)) {
        $foundCli = Get-ChildItem -LiteralPath $Extract -Filter "akasha.exe" -File -Recurse | Select-Object -First 1
        if ($null -eq $foundCli) {
            throw "akasha.exe is missing from the archive"
        }
        $Cli = $foundCli.FullName
    }
    $Root = Split-Path -Parent $Cli
    $DaemonPath = Join-Path $Root "akasha-daemon.exe"
    if (-not (Test-Path -LiteralPath $DaemonPath -PathType Leaf)) {
        throw "akasha-daemon.exe is missing beside akasha.exe"
    }
    foreach ($relative in @("spec\09_event_model.yaml", "spec\10_data_model.yaml")) {
        if (-not (Test-Path -LiteralPath (Join-Path $Root $relative) -PathType Leaf)) {
            throw "release archive is missing $relative"
        }
    }

    $versionText = (& $Cli --version 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) {
        throw "akasha --version failed: $versionText"
    }
    if ($ExpectedVersion -and $versionText -notmatch [regex]::Escape($ExpectedVersion)) {
        throw "expected package version $ExpectedVersion; got $versionText"
    }
    Write-SmokeLog "version=$versionText archive=$([IO.Path]::GetFileName($ArtifactPath))"

    $env:AKASHA_DATA_DIR = $DataDir
    $env:AKASHA_BIND = "127.0.0.1"
    $env:AKASHA_LANG = "en"
    $env:OLLAMA_HOST = "http://127.0.0.1:1"
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $listener.Dispose()
    $env:AKASHA_PORT = "$port"

    Push-Location $Root
    try {
        $initOutput = (& $Cli init --defaults 2>&1 | Out-String).Trim()
        Add-Content -LiteralPath $Log -Value $initOutput -Encoding utf8
        if ($LASTEXITCODE -ne 0) {
            throw "akasha init --defaults failed with exit code $LASTEXITCODE"
        }
        Write-SmokeLog "init=passed"

        $Daemon = Start-Process -FilePath $DaemonPath -WorkingDirectory $Root -PassThru -WindowStyle Hidden `
            -RedirectStandardOutput $DaemonOut -RedirectStandardError $DaemonErr
        $base = "http://127.0.0.1:$port"
        $deadline = [DateTime]::UtcNow.AddSeconds(90)
        $healthy = $false
        while ([DateTime]::UtcNow -lt $deadline) {
            if ($Daemon.HasExited) {
                throw "daemon exited before becoming healthy (code $($Daemon.ExitCode))"
            }
            try {
                $status = Invoke-RestMethod -Uri "$base/api/status" -TimeoutSec 3
                if ($status.status -eq "ok") {
                    $healthy = $true
                    break
                }
            } catch { }
            Start-Sleep -Milliseconds 500
        }
        if (-not $healthy) {
            throw "daemon did not become healthy within 90 seconds"
        }
        Write-SmokeLog "daemon_ready=true port=$port"

        $doctorOutput = (& $Cli doctor 2>&1 | Out-String).Trim()
        Set-Content -LiteralPath $DoctorLog -Value $doctorOutput -Encoding utf8
        Add-Content -LiteralPath $Log -Value $doctorOutput -Encoding utf8
        if ($LASTEXITCODE -ne 0) {
            throw "akasha doctor failed with exit code $LASTEXITCODE"
        }
        if ($doctorOutput -notmatch '(?m)^\s*All checks passed\.\s*$') {
            throw "akasha doctor exited successfully but did not report all checks passed"
        }
        Write-SmokeLog "doctor=passed"

        $embedded = Invoke-RestMethod -Uri "$base/api/router/embedded-status" -TimeoutSec 10
        $embedded | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $EmbeddedLog -Encoding utf8
        if ($embedded.embedded_available -ne $true) {
            throw "embedded model is not available: $($embedded | ConvertTo-Json -Compress -Depth 8)"
        }

        $started = [DateTime]::UtcNow
        $body = @{ message = "Réponds uniquement par le mot OK."; composer_mode = "technical_writer" } | ConvertTo-Json -Compress
        $created = Invoke-RestMethod -Uri "$base/api/message" -Method Post -ContentType "application/json" -Body $body -TimeoutSec 30
        $taskId = [string]$created.task_id
        if ([string]::IsNullOrWhiteSpace($taskId)) {
            throw "POST /api/message returned no task_id"
        }
        Write-SmokeLog "task_id=$taskId"

        $deadline = $started.AddSeconds($InferenceTimeoutSecs)
        $task = $null
        while ([DateTime]::UtcNow -lt $deadline) {
            Start-Sleep -Seconds 2
            $task = Invoke-RestMethod -Uri "$base/api/tasks/$taskId" -TimeoutSec 15
            $task | ConvertTo-Json -Depth 24 | Set-Content -LiteralPath $TaskLog -Encoding utf8
            if ($task.status -in @("done", "completed")) {
                break
            }
            if ($task.status -in @("failed", "error")) {
                throw "model task failed: $($task | ConvertTo-Json -Compress -Depth 12)"
            }
        }
        if ($null -eq $task -or $task.status -notin @("done", "completed")) {
            throw "first inference timed out after $InferenceTimeoutSecs seconds"
        }
        if ($task.assigned_agent -ne "technical_writer") {
            throw "wrong assigned agent: $($task.assigned_agent)"
        }
        if ($task.last_turn_model_used -ne "embedded") {
            throw "embedded model was not used: $($task.last_turn_model_used)"
        }
        if ([long]$task.last_turn_tokens_out -le 0) {
            throw "model returned no completion tokens"
        }
        $messages = @($task.progress | Where-Object { $_.message -is [string] } | ForEach-Object {
            [regex]::Replace($_.message, "<think>.*?</think>", "", [System.Text.RegularExpressions.RegexOptions]::Singleline).Trim()
        } | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        $reply = if ($messages.Count -gt 0) { $messages[-1] } else { "" }
        $normalizedReply = ($reply -split "\s+" -join " ").Trim()
        if ($normalizedReply -ne "OK") {
            throw "final model reply must be exactly 'OK'; got '$($normalizedReply.Substring(0, [Math]::Min(160, $normalizedReply.Length)))'"
        }
        $elapsed = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 2)
        Write-SmokeLog "first_inference=passed model=embedded elapsed_seconds=$elapsed reply=OK"
        Write-SmokeLog "smoke=passed os=$([Environment]::OSVersion.VersionString)"
    }
    finally {
        Pop-Location
    }
}
catch {
    Write-SmokeLog "::error::$($_.Exception.Message)"
    if ($null -ne $Daemon -and -not $Daemon.HasExited) {
        Get-Content -LiteralPath $DaemonOut -Tail 80 -ErrorAction SilentlyContinue | Add-Content -LiteralPath $Log -Encoding utf8
        Get-Content -LiteralPath $DaemonErr -Tail 80 -ErrorAction SilentlyContinue | Add-Content -LiteralPath $Log -Encoding utf8
    }
    Copy-Evidence
    exit 1
}
finally {
    if ($null -ne $Daemon -and -not $Daemon.HasExited) {
        Stop-Process -Id $Daemon.Id -Force -ErrorAction SilentlyContinue
    }
    Copy-Evidence
    $tempRoot = [IO.Path]::GetFullPath($env:TEMP).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    $workFull = [IO.Path]::GetFullPath($Work)
    if (-not $workFull.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to remove a smoke directory outside TEMP: $workFull"
    }
    Remove-Item -LiteralPath $workFull -Recurse -Force -ErrorAction SilentlyContinue
}
