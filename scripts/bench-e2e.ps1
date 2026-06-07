# Quick E2E smoke for CI / local (Hermes parity pack — minimal).
# Extend with your own scenarios (TTFB, p95) using hyperfine or wrk.
$ErrorActionPreference = "Stop"
$port = if ($env:AKASHA_PORT) { $env:AKASHA_PORT } else { "3876" }
$base = "http://127.0.0.1:$port"

Write-Host "GET $base/ ..."
Measure-Command { Invoke-WebRequest -Uri "$base/" -UseBasicParsing | Out-Null } | Select-Object TotalMilliseconds

Write-Host "GET $base/api/tools/effective ..."
Measure-Command { Invoke-WebRequest -Uri "$base/api/tools/effective" -UseBasicParsing | Out-Null } | Select-Object TotalMilliseconds

Write-Host "GET $base/api/memory/recall-metrics ..."
Measure-Command { Invoke-WebRequest -Uri "$base/api/memory/recall-metrics" -UseBasicParsing | Out-Null } | Select-Object TotalMilliseconds

Write-Host "GET $base/api/plugins/metrics ..."
Measure-Command { Invoke-WebRequest -Uri "$base/api/plugins/metrics" -UseBasicParsing | Out-Null } | Select-Object TotalMilliseconds

Write-Host "Done."
