# S-MEM-05 — SQLCipher / field-at-rest encryption spike (stub)
#
# Documents bench steps before enabling SQLCipher or per-field AES-GCM on memory.db.
# See: spec/dev/roadmap/memory_encryption_rfc.md
#
# Prerequisites:
#   1. akasha doctor --fix --encrypt-memory   (sets AKASHA_MEMORY_ENCRYPT=1 in akasha.env)
#   2. Daemon running on AKASHA_PORT (default 3876)
#   3. Synthetic memory dataset (10k entries) — seed via export/import or test harness
#
# Spike steps (manual until SQLCipher feature flag lands):
#   A. Baseline recall latency (plaintext memory.db)
#      - Run scripts/bench-fts5-recall.ps1 and note avg/p95
#   B. SQLCipher prototype (feature flag memory-encryption)
#      - Build: cargo build -p akasha-daemon --features memory-encryption  # TBD
#      - Open memory.db with PRAGMA key; re-run bench-fts5-recall.ps1
#   C. Per-field encryption alternative
#      - Bench decrypt-on-recall for top-20 semantic hits (CPU overhead)
#   D. Accept if p95 recall latency regression <= 15% vs baseline
#
# Operator checks:
#   - strings ~/akasha/memory.db | head     # should show gibberish when encrypted
#   - akasha doctor                           # encryption state advisory when AKASHA_MEMORY_ENCRYPT=1

param(
  [string]$DaemonUrl = "http://127.0.0.1:3876",
  [string]$Query = "memory recall benchmark",
  [int]$TopK = 20,
  [int]$Iterations = 20
)

$ErrorActionPreference = "Stop"

Write-Host "=== Memory encryption spike (stub) ===" -ForegroundColor Cyan
Write-Host ""
Write-Host "This script delegates baseline recall timing to bench-fts5-recall.ps1."
Write-Host "Full SQLCipher integration is deferred — see memory_encryption_rfc.md Phase A."
Write-Host ""

$benchScript = Join-Path $PSScriptRoot "bench-fts5-recall.ps1"
if (-not (Test-Path $benchScript)) {
  Write-Error "Missing $benchScript"
  exit 1
}

& $benchScript -DaemonUrl $DaemonUrl -Query $Query -TopK $TopK -Iterations $Iterations

Write-Host ""
Write-Host "Next steps:" -ForegroundColor Yellow
Write-Host "  1. Enable AKASHA_MEMORY_ENCRYPT=1 via: akasha doctor --fix --encrypt-memory"
Write-Host "  2. Implement SQLCipher or per-field encryption in akasha-store/long_term_memory.rs"
Write-Host "  3. Re-run this script and compare p95 to baseline (target: <=15% regression)"
