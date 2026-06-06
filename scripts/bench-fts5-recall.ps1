param(
  [string]$DaemonUrl = "http://127.0.0.1:3876",
  [string]$Query = "memory recall benchmark",
  [int]$TopK = 10,
  [int]$Iterations = 20
)

$ErrorActionPreference = "Stop"

Write-Host "Benchmarking /api/memory/search ($Iterations iterations)..."
$times = @()
for ($i = 1; $i -le $Iterations; $i++) {
  $sw = [System.Diagnostics.Stopwatch]::StartNew()
  try {
    $uri = "$DaemonUrl/api/memory/search?q=$([uri]::EscapeDataString($Query))&top_k=$TopK"
    $null = Invoke-RestMethod -Uri $uri -Method Get -TimeoutSec 30
  } catch {
    Write-Warning "Iteration $i failed: $($_.Exception.Message)"
    continue
  } finally {
    $sw.Stop()
  }
  $times += $sw.ElapsedMilliseconds
}

if ($times.Count -eq 0) {
  Write-Error "No successful iteration."
  exit 1
}

$avg = [Math]::Round((($times | Measure-Object -Average).Average), 2)
$p95Index = [Math]::Max([Math]::Ceiling($times.Count * 0.95) - 1, 0)
$sorted = $times | Sort-Object
$p95 = $sorted[$p95Index]

Write-Host "Samples: $($times.Count)"
Write-Host "Avg ms : $avg"
Write-Host "P95 ms : $p95"
