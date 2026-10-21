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
$EmbeddedAfterLog = Join-Path $Work "embedded-status-after.json"
$UiLog = Join-Path $Work "ui.log"
$UiInstallerLog = Join-Path $Work "ui-msi-extract.log"
$DaemonOut = Join-Path $Work "daemon.stdout.log"
$DaemonErr = Join-Path $Work "daemon.stderr.log"
$Daemon = $null
$UiProcess = $null

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
    foreach ($path in @($Log, $DoctorLog, $TaskLog, $EmbeddedLog, $EmbeddedAfterLog, $UiLog, $UiInstallerLog, $DaemonOut, $DaemonErr, (Join-Path $DataDir "llm_router.yaml"))) {
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
    $env:AKASHA_EMBEDDED_BACKEND = "candle"
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
        $routerPath = Join-Path $DataDir "llm_router.yaml"
        if (-not (Select-String -LiteralPath $routerPath -Pattern '^\s*provider:\s*akasha_embedded\s*$' -Quiet)) {
            throw "first-run configuration does not route to akasha_embedded"
        }
        Write-SmokeLog "embedded_primary_route=passed"
        Write-SmokeLog "init=passed"

        # Candle's small first-run model can echo the default tool catalog into a
        # deterministic health reply. Keep this smoke focused on inference only.
        @'
default_profile: release_smoke_no_tools
tool_profiles:
  release_smoke_no_tools: []
allowed_read_paths: []
allowed_write_paths: []
allowed_commands: []
web_search_enabled: false
web_crawl_enabled: false
browser_enabled: false
'@ | Set-Content -LiteralPath (Join-Path $DataDir "tools_policy.yaml") -Encoding utf8NoBOM
        Write-SmokeLog "smoke_tool_policy=restricted"

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
        $body = @{ message = "Return only the exact text AKASHA_OK. Do not inspect, read, or write files. Do not use or call any tools."; composer_mode = "technical_writer" } | ConvertTo-Json -Compress
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
            $taskEvidence = $task | ConvertTo-Json -Depth 24 | ConvertFrom-Json
            $thinkOptions = [System.Text.RegularExpressions.RegexOptions]::Singleline -bor [System.Text.RegularExpressions.RegexOptions]::IgnoreCase
            foreach ($entry in $taskEvidence.progress) {
                if ($entry.message -is [string]) {
                    $entry.message = [regex]::Replace($entry.message, "<think>.*?(?:</think>|$)", "", $thinkOptions).Trim()
                }
            }
            if (@($taskEvidence.progress | Where-Object { $_.message -match "<think>" }).Count -gt 0) {
                throw "refusing to preserve unredacted model reasoning in release diagnostics"
            }
            $taskEvidence | ConvertTo-Json -Depth 24 | Set-Content -LiteralPath $TaskLog -Encoding utf8
            if ($task.status -in @("done", "completed")) {
                break
            }
            if ($task.status -in @("failed", "error")) {
                throw "model task failed: $($taskEvidence | ConvertTo-Json -Compress -Depth 12)"
            }
        }
        if ($null -eq $task -or $task.status -notin @("done", "completed")) {
            throw "first inference timed out after $InferenceTimeoutSecs seconds"
        }
        if ($task.assigned_agent -ne "technical_writer") {
            throw "wrong assigned agent: $($task.assigned_agent)"
        }
        if ($task.last_turn_model_used -ne "default") {
            throw "embedded model was not used: $($task.last_turn_model_used)"
        }
        $messages = @($task.progress | Where-Object { $_.message -is [string] } | ForEach-Object {
            [regex]::Replace($_.message, "<think>.*?</think>", "", [System.Text.RegularExpressions.RegexOptions]::Singleline).Trim()
        } | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        $reply = if ($messages.Count -gt 0) { $messages[-1] } else { "" }
        $normalizedReply = ($reply -split "\s+" -join " ").Trim()
        if ($normalizedReply -notmatch '(?:^|\s)AKASHA_OK[.!]?$') {
            throw "final model reply must end with the requested code word; got '$($normalizedReply.Substring(0, [Math]::Min(160, $normalizedReply.Length)))'"
        }
        $embeddedAfter = Invoke-RestMethod -Uri "$base/api/router/embedded-status" -TimeoutSec 10
        $embeddedAfter | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $EmbeddedAfterLog -Encoding utf8
        if ($embeddedAfter.embedded_loaded -ne $true -or $embeddedAfter.backend -ne "candle") {
            throw "first inference did not load the CPU Candle backend: $($embeddedAfter | ConvertTo-Json -Compress -Depth 8)"
        }
        $elapsed = [Math]::Round(([DateTime]::UtcNow - $started).TotalSeconds, 2)
        Write-SmokeLog "first_inference=passed model=default backend=candle tokens_out=$($task.last_turn_tokens_out) elapsed_seconds=$elapsed reply_marker=AKASHA_OK"

        $uiMsiDir = Join-Path $Root "ui\msi"
        $uiMsi = Get-ChildItem -LiteralPath $uiMsiDir -Filter "Akasha_*.msi" -File -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($null -eq $uiMsi) {
            throw "Windows desktop UI MSI is missing from $uiMsiDir"
        }
        $uiExtract = Join-Path $Work "ui-extract"
        New-Item -ItemType Directory -Path $uiExtract -Force | Out-Null
        $msiArguments = @(
            "/a", "`"$($uiMsi.FullName)`"", "/qn", "TARGETDIR=`"$uiExtract`"", "/norestart", "/l*v", "`"$UiInstallerLog`""
        )
        $installer = Start-Process -FilePath "msiexec.exe" -ArgumentList $msiArguments -PassThru -Wait -WindowStyle Hidden
        if ($installer.ExitCode -ne 0) {
            throw "Windows desktop UI MSI administrative extraction failed with code $($installer.ExitCode)"
        }
        $uiExe = Get-ChildItem -LiteralPath $uiExtract -Filter "akasha-ui.exe" -File -Recurse | Select-Object -First 1
        if ($null -eq $uiExe) {
            throw "akasha-ui.exe is missing from the extracted Windows UI MSI"
        }
        $uiStarted = [DateTime]::UtcNow
        $UiProcess = Start-Process -FilePath $uiExe.FullName -WorkingDirectory $uiExe.DirectoryName -PassThru
        $uiDeadline = [DateTime]::UtcNow.AddSeconds(30)
        $uiTitle = ""
        $uiHandle = [IntPtr]::Zero
        while ([DateTime]::UtcNow -lt $uiDeadline) {
            $UiProcess.Refresh()
            if ($UiProcess.HasExited) {
                throw "Windows desktop UI exited before creating a window (code $($UiProcess.ExitCode))"
            }
            $uiTitle = $UiProcess.MainWindowTitle
            $uiHandle = $UiProcess.MainWindowHandle
            if ($uiHandle -ne [IntPtr]::Zero -and $uiTitle -match "Akasha") {
                break
            }
            Start-Sleep -Milliseconds 500
        }
        if ($uiHandle -eq [IntPtr]::Zero -or $uiTitle -notmatch "Akasha") {
            throw "Windows desktop UI did not create an Akasha window (title='$uiTitle')"
        }
        $uiElapsed = [Math]::Round(([DateTime]::UtcNow - $uiStarted).TotalSeconds, 2)
        Set-Content -LiteralPath $UiLog -Value "window_title=$uiTitle`nlaunch_seconds=$uiElapsed`nexecutable=$($uiExe.FullName)" -Encoding utf8
        Write-SmokeLog "ui_launch=passed platform=Windows title=$uiTitle elapsed_seconds=$uiElapsed"
        $null = $UiProcess.CloseMainWindow()
        if (-not $UiProcess.WaitForExit(5000)) {
            Stop-Process -Id $UiProcess.Id -Force -ErrorAction SilentlyContinue
        }
        $UiProcess = $null
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
    if ($null -ne $UiProcess -and -not $UiProcess.HasExited) {
        Stop-Process -Id $UiProcess.Id -Force -ErrorAction SilentlyContinue
    }
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
