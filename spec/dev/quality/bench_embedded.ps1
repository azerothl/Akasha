# Embedded model bench (CPU/CUDA) — TTFT proxy, throughput, end-to-end.
param(
    [ValidateSet("auto", "candle", "llama_cpp")]
    [string]$Backend = "auto",
    [int]$Port = 3876,
    [string]$Prompt = "Write exactly 80 short numbered lines, one per line, starting at 1.",
    [int]$MaxTokens = 256,
    [switch]$Json,
    [switch]$Strict
)

$ErrorActionPreference = "Stop"
$base = "http://127.0.0.1:$Port"

function Fail($msg) {
    if ($Json) {
        @{ ok = $false; error = $msg } | ConvertTo-Json -Depth 4
    } else {
        Write-Error $msg
    }
    exit 1
}

$status = Invoke-RestMethod -Uri "$base/api/router/embedded-status" -Method Get
if (-not $status.embedded_available) { Fail "Embedded LLM not available" }

if ($Backend -ne "auto") {
    $env:AKASHA_EMBEDDED_BACKEND = $Backend
}

$body = @{
    message = $Prompt
    provider = "akasha_embedded"
    max_tokens = $MaxTokens
} | ConvertTo-Json

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$task = Invoke-RestMethod -Uri "$base/api/message" -Method Post -Body $body -ContentType "application/json"
$taskId = $task.task_id
if (-not $taskId) { Fail "No task_id" }

$reply = $null
$firstProgressAt = $null
while ($true) {
    Start-Sleep -Milliseconds 200
    $t = Invoke-RestMethod -Uri "$base/api/tasks/$taskId" -Method Get
    if ($t.progress -and $t.progress.Count -gt 0 -and -not $firstProgressAt) {
        $firstProgressAt = $sw.Elapsed.TotalSeconds
    }
    if ($t.status -eq "done" -or $t.status -eq "completed") {
        $reply = $t.result
        break
    }
    if ($t.status -eq "failed") { Fail "Task failed: $($t.error)" }
}
$sw.Stop()

$text = if ($reply -is [string]) { $reply } else { $reply.text ?? $reply.ToString() }
$approxTokens = [math]::Max(1, ($text -split '\s+').Count)
$secs = [math]::Max(0.001, $sw.Elapsed.TotalSeconds)
$tokPerSec = [math]::Round($approxTokens / $secs, 1)
$ttft = if ($firstProgressAt) { [math]::Round($firstProgressAt, 2) } else { $null }

$result = [ordered]@{
    ok = $true
    backend = $status.backend
    device = $status.device
    embedded_loaded = $status.embedded_loaded
    duration_s = [math]::Round($secs, 2)
    approx_tokens = $approxTokens
    tok_per_s = $tokPerSec
    ttft_s = $ttft
    date = (Get-Date -Format "yyyy-MM-dd")
}

if ($Strict -and $status.device -match "cuda" -and $tokPerSec -lt 20) {
    $result.ok = $false
    $result.error = "CUDA throughput below 20 tok/s gate"
}

if ($Json) {
    $result | ConvertTo-Json -Depth 4
} else {
    $result.GetEnumerator() | ForEach-Object { Write-Host ("{0}: {1}" -f $_.Key, $_.Value) }
}

if (-not $result.ok) { exit 1 }
