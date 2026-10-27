param([string]$OutputFile = $env:GITHUB_ENV, [string]$TemporaryRoot = $env:RUNNER_TEMP)
$ErrorActionPreference = 'Stop'
if (-not $OutputFile -or -not $TemporaryRoot) { throw 'A runner temp directory and environment output are required.' }
$root = [IO.Path]::GetFullPath($TemporaryRoot)
New-Item -ItemType Directory -Path $root -Force | Out-Null
$profile = Join-Path $root ("akasha-gpu-ci-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $profile | Out-Null
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
try { $listener.Start(); $port = $listener.LocalEndpoint.Port } finally { $listener.Stop() }
if (-not $env:CUDA_PATH -or -not (Test-Path (Join-Path $env:CUDA_PATH 'bin/nvcc.exe'))) {
    throw 'CUDA_PATH must select an installed NVIDIA toolkit.'
}
$capability = (& nvidia-smi --query-gpu=compute_cap --format=csv,noheader | Select-Object -First 1).Trim()
if ($LASTEXITCODE -or $capability -notmatch '^\d+\.\d+$') { throw 'Cannot identify the NVIDIA compute capability.' }
@("AKASHA_DATA_DIR=$profile", "AKASHA_PORT=$port", "AKASHA_GPU_RUNNER_PROFILE=$profile",
  "CMAKE_CUDA_ARCHITECTURES=$($capability.Replace('.', ''))") | Add-Content -LiteralPath $OutputFile -Encoding utf8
@((Join-Path $env:CUDA_PATH 'bin'), (Join-Path $env:CUDA_PATH 'bin/x64')) |
    Where-Object { Test-Path -LiteralPath $_ } | Add-Content -LiteralPath $env:GITHUB_PATH -Encoding utf8
Write-Host "GPU test profile prepared on a private loopback port ($port)."
