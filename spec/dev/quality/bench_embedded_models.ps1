# Multi-model embedded bench — restarts daemon per GGUF, measures TTFT + tok/s.
param(
    [string[]]$Models = @(),
    [switch]$IncludeCandle,
    [switch]$Matrix,
    [string]$SimulatedTier = "gpu_low_4gb",
    [int[]]$NglValues = @(0, 99),
    [int]$Port = 3876,
    [int]$MaxTokens = 128,
    [string]$Prompt = "Write exactly 60 short numbered lines, one per line, starting at 1.",
    [string]$DaemonExe = "c:\www\Akasha\target\x86_64-pc-windows-msvc\release\akasha-daemon.exe",
    [string]$ResultsFile = "c:\www\Akasha\spec\dev\quality\bench_embedded_results_run.json",
    [string]$BaselinesFile = "c:\www\Akasha\spec\dev\quality\embedded_profile_baselines.json"
)

$ErrorActionPreference = "Stop"
$base = "http://127.0.0.1:$Port"

function Stop-Daemon {
    Get-Process akasha-daemon -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Seconds 2
}

function Start-DaemonWithEnv {
    param([hashtable]$EnvVars)
    Stop-Daemon
    $env:AKASHA_EMBEDDED_BACKEND = $EnvVars.Backend
    if ($EnvVars.GgufPath) { $env:AKASHA_EMBEDDED_GGUF_PATH = $EnvVars.GgufPath }
    else { Remove-Item Env:AKASHA_EMBEDDED_GGUF_PATH -ErrorAction SilentlyContinue }
    if ($null -ne $EnvVars.Ngl) { $env:AKASHA_EMBEDDED_N_GPU_LAYERS = [string]$EnvVars.Ngl }
    else { Remove-Item Env:AKASHA_EMBEDDED_N_GPU_LAYERS -ErrorAction SilentlyContinue }
    $proc = Start-Process -FilePath $DaemonExe -PassThru -WindowStyle Hidden
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        try {
            $null = Invoke-RestMethod -Uri "$base/api/status" -Method Get -TimeoutSec 2
            return $proc
        } catch { Start-Sleep -Milliseconds 500 }
    }
    throw "Daemon failed to start within 30s"
}

function Invoke-EmbeddedBench {
    param([string]$ModelName, [string]$Backend, [string]$GgufPath, [int]$Ngl = 99)

    $proc = Start-DaemonWithEnv @{ Backend = $Backend; GgufPath = $GgufPath; Ngl = $Ngl }
    try {
        $status = Invoke-RestMethod -Uri "$base/api/router/embedded-status" -Method Get
        $body = @{
            message = $Prompt
            provider = "akasha_embedded"
            max_tokens = $MaxTokens
        } | ConvertTo-Json

        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $task = Invoke-RestMethod -Uri "$base/api/message" -Method Post -Body $body -ContentType "application/json"
        $taskId = $task.task_id
        if (-not $taskId) { throw "No task_id" }

        $firstProgressAt = $null
        $reply = $null
        $genStart = $null
        while ($true) {
            Start-Sleep -Milliseconds 200
            $t = Invoke-RestMethod -Uri "$base/api/tasks/$taskId" -Method Get
            if ($t.progress -and $t.progress.Count -gt 0 -and -not $firstProgressAt) {
                $firstProgressAt = $sw.Elapsed.TotalSeconds
                $genStart = $sw.Elapsed.TotalSeconds
            }
            if ($t.status -in @("done", "completed")) {
                $reply = if ($t.result) { $t.result } elseif ($t.reply) { $t.reply } else { $null }
                break
            }
            if ($t.status -eq "failed") { throw "Task failed: $($t.error)" }
            if ($sw.Elapsed.TotalSeconds -gt 600) { throw "Timeout after 600s" }
        }
        $sw.Stop()

        $text = if ($reply -is [string]) { $reply } elseif ($reply.text) { $reply.text } else { "" }
        $approxTokens = [math]::Max(1, ($text -split '\s+').Where({ $_ }).Count)
        $secs = [math]::Max(0.001, $sw.Elapsed.TotalSeconds)
        $genSecs = if ($genStart) { [math]::Max(0.001, $secs - $genStart) } else { $secs }
        $tokPerSec = [math]::Round($approxTokens / $genSecs, 1)
        $tokPerSecE2e = [math]::Round($approxTokens / $secs, 1)

        return [ordered]@{
            model = $ModelName
            backend = $status.backend ?? $Backend
            device = $status.device
            gguf_path = $GgufPath
            n_gpu_layers = $Ngl
            duration_s = [math]::Round($secs, 2)
            gen_duration_s = [math]::Round($genSecs, 2)
            ttft_s = if ($firstProgressAt) { [math]::Round($firstProgressAt, 2) } else { $null }
            approx_tokens = $approxTokens
            tok_per_s = $tokPerSec
            tok_per_s_e2e = $tokPerSecE2e
            ok = $true
            date = (Get-Date -Format "yyyy-MM-dd")
        }
    } finally {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
    }
}

$defaultModels = @(
    @{ name = "Qwen2.5-1.5B Q4 (prod)"; path = "$env:USERPROFILE\akasha\models\embedded\default.gguf" }
    @{ name = "SmolLM2-360M Q4"; path = "$env:USERPROFILE\akasha\models\embedded\bench\smollm2-360m.gguf" }
    @{ name = "Qwen3-0.6B GGUF Q8"; path = "$env:USERPROFILE\akasha\models\embedded\bench\qwen3-0.6b.gguf" }
    @{ name = "Qwen3.5-0.8B Q4"; path = "$env:USERPROFILE\akasha\models\embedded\bench\qwen3.5-0.8b.gguf" }
    @{ name = "Gemma 3 1B QAT Q4"; path = "$env:USERPROFILE\akasha\models\embedded\bench\gemma3-1b.gguf" }
    @{ name = "Qwen3-1.7B Q4"; path = "$env:USERPROFILE\akasha\models\embedded\bench\qwen3-1.7b.gguf" }
)

if ($Models.Count -gt 0) {
    $defaultModels = $defaultModels | Where-Object { $Models -contains $_.name }
}

$results = @()
foreach ($m in $defaultModels) {
    if (-not (Test-Path $m.path)) {
        Write-Host "SKIP $($m.name) — missing $($m.path)"
        $results += [ordered]@{ model = $m.name; ok = $false; error = "file missing" }
        continue
    }
    $sizeMb = [math]::Round((Get-Item $m.path).Length / 1MB, 1)
    if ($sizeMb -lt 10) {
        Write-Host "SKIP $($m.name) — corrupt file (${sizeMb} MB)"
        $results += [ordered]@{ model = $m.name; ok = $false; error = "corrupt download (${sizeMb} MB)" }
        continue
    }
    Write-Host "BENCH $($m.name) (${sizeMb} MB)..."
    try {
        if ($Matrix) {
            foreach ($ngl in $NglValues) {
                Write-Host "  ngl=$ngl ..."
                $r = Invoke-EmbeddedBench -ModelName "$($m.name) ngl=$ngl" -Backend "llama_cpp" -GgufPath $m.path -Ngl $ngl
                $r.tier_simulated = $SimulatedTier
                $results += $r
                Write-Host "    -> $($r.tok_per_s) tok/s  device=$($r.device)"
            }
        } else {
            $r = Invoke-EmbeddedBench -ModelName $m.name -Backend "llama_cpp" -GgufPath $m.path
            $results += $r
            Write-Host "  -> $($r.tok_per_s) tok/s (gen)  TTFT=$($r.ttft_s)s  device=$($r.device)"
        }
    } catch {
        Write-Host "  FAIL: $_"
        $results += [ordered]@{ model = $m.name; ok = $false; error = $_.Exception.Message }
    }
}

if ($IncludeCandle) {
    Write-Host "BENCH Qwen3-0.6B Candle (CPU)..."
    try {
        $r = Invoke-EmbeddedBench -ModelName "Qwen3-0.6B Candle ST" -Backend "candle" -GgufPath $null
        $results += $r
        Write-Host "  -> $($r.tok_per_s) tok/s  TTFT=$($r.ttft_s)s"
    } catch {
        Write-Host "  FAIL: $_"
        $results += [ordered]@{ model = "Qwen3-0.6B Candle ST"; ok = $false; error = $_.Exception.Message }
    }
}

$machine = @{
    gpu = (nvidia-smi --query-gpu=name,memory.total --format=csv,noheader 2>$null)
    cpu = (Get-CimInstance Win32_Processor -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Name)
    ram_gb = [math]::Round((Get-CimInstance Win32_ComputerSystem -ErrorAction SilentlyContinue).TotalPhysicalMemory / 1GB)
}
$out = @{
    machine = $machine
    results = $results
    prompt = $Prompt
    max_tokens = $MaxTokens
    matrix = [bool]$Matrix
    tier_simulated = if ($Matrix) { $SimulatedTier } else { $null }
}
$out | ConvertTo-Json -Depth 6 | Set-Content -Path $ResultsFile -Encoding UTF8
Write-Host "`nResults written to $ResultsFile"
if ($Matrix) {
    $baseline = @{
        version = 1
        generated_at = (Get-Date -Format "yyyy-MM-dd")
        tier_id = $SimulatedTier
        machine = $machine
        matrix = $results | Where-Object { $_.ok }
    }
    $baseline | ConvertTo-Json -Depth 6 | Set-Content -Path $BaselinesFile -Encoding UTF8
    Write-Host "Baselines written to $BaselinesFile"
}
$results | Format-Table model, backend, device, tok_per_s, ttft_s, duration_s, ok -AutoSize
