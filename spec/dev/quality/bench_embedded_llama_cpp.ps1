# Manual throughput bench for embedded llama-cpp backend (v1 acceptance: >20 tok/s on GTX 3080+).
# Prerequisites: daemon built with embedded-llama-cpp-cuda, GGUF at default path, daemon running.
param(
    [int]$Port = 3876,
    [string]$Prompt = "Write exactly 80 short numbered lines, one per line, starting at 1.",
    [int]$MaxTokens = 256
)

$base = "http://127.0.0.1:$Port"
Write-Host "Checking embedded status..."
$status = Invoke-RestMethod -Uri "$base/api/router/embedded-status" -Method Get
$status | ConvertTo-Json -Depth 4
if (-not $status.embedded_available) {
    Write-Error "Embedded LLM not available. Build with embedded-llama-cpp and download GGUF."
    exit 1
}

$body = @{
    message = $Prompt
    provider = "akasha_embedded"
    max_tokens = $MaxTokens
} | ConvertTo-Json

Write-Host "Sending message (max_tokens=$MaxTokens)..."
$sw = [System.Diagnostics.Stopwatch]::StartNew()
$task = Invoke-RestMethod -Uri "$base/api/message" -Method Post -Body $body -ContentType "application/json"
$taskId = $task.task_id
if (-not $taskId) {
    Write-Error "No task_id in response"
    exit 1
}

$reply = $null
while ($true) {
    Start-Sleep -Milliseconds 200
    $t = Invoke-RestMethod -Uri "$base/api/tasks/$taskId" -Method Get
    if ($t.status -eq "done") {
        $reply = $t.result
        break
    }
    if ($t.status -eq "failed") {
        Write-Error "Task failed: $($t.error)"
        exit 1
    }
}
$sw.Stop()

$text = if ($reply -is [string]) { $reply } else { $reply.text ?? $reply.ToString() }
$approxTokens = [math]::Max(1, ($text -split '\s+').Count)
$secs = [math]::Max(0.001, $sw.Elapsed.TotalSeconds)
$tokPerSec = [math]::Round($approxTokens / $secs, 1)

Write-Host ""
Write-Host "Duration: $([math]::Round($secs, 2)) s"
Write-Host "Approx tokens (word split): $approxTokens"
Write-Host "Approx throughput: $tokPerSec tok/s"
Write-Host "Backend: $($status.backend)  Device: $($status.device)"
