# Build akasha-daemon with embedded-llama-cpp-cuda on Windows (MSVC).
# Used locally and by .github/workflows/release.yml (akasha-windows-x86_64-cuda).
#
# llama-cpp-sys builds with /MD (dynamic CRT) on Windows; nvcc host code must match
# (/MD via NVCC_PREPEND_FLAGS). Do not use +crt-static here — linking /MT Rust with
# /MD llama-cpp objects produces LNK2019 on __imp_* symbols.

param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$Features = "embedded,embeddings-tract,embedded-baguettotron,embedded-llama-cpp,embedded-llama-cpp-cuda",
    [switch]$CleanLlamaCache
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $ProjectRoot

if (-not $env:CUDA_PATH) {
    throw "CUDA_PATH is not set. Install the CUDA toolkit or set CUDA_PATH."
}

if (-not $env:LIBCLANG_PATH) {
    $llvmBin = "C:\Program Files\LLVM\bin"
    if (Test-Path (Join-Path $llvmBin "libclang.dll")) {
        $env:LIBCLANG_PATH = $llvmBin
    } else {
        throw "libclang not found. Install LLVM and set LIBCLANG_PATH (e.g. C:\Program Files\LLVM\bin)."
    }
}

# Dynamic CRT to match llama-cpp-sys (default /MD) and nvcc host compilation.
# esaxx-rs is patched in vendor/ (see Cargo.toml [patch.crates-io]) to default /MD on MSVC.
Remove-Item Env:LLAMA_STATIC_CRT -ErrorAction SilentlyContinue
$env:CMAKE_MSVC_RUNTIME_LIBRARY = "MultiThreadedDLL"
$env:NVCC_PREPEND_FLAGS = "-Xcompiler /MD"

# CUDA 12.9 rejects newer MSVC on windows-latest during CMakeCUDACompilerId.cu (C1189).
# Override until toolkit and runner toolsets are aligned.
$env:CMAKE_CUDA_FLAGS = "-allow-unsupported-compiler"
$env:CUDAFLAGS = "-allow-unsupported-compiler"

$cudaLibCandidates = @(
    (Join-Path $env:CUDA_PATH "lib\x64"),
    (Join-Path $env:CUDA_PATH "lib64")
)
$cudaLib = $cudaLibCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $cudaLib) {
    throw "CUDA library directory not found under $env:CUDA_PATH (tried: $($cudaLibCandidates -join ', '))."
}

# ggml-cuda needs driver + runtime + cuBLAS at final link (static libs do not propagate them).
$sep = [char]0x1f
$encodedParts = @(
    "-L", "native=$cudaLib",
    "-l", "cuda",
    "-l", "cudart",
    "-l", "cublas",
    "-l", "cublasLt"
)
$env:CARGO_ENCODED_RUSTFLAGS = ($encodedParts -join $sep)

$gitUsrBin = Join-Path $env:ProgramFiles "Git\usr\bin"
if (Test-Path $gitUsrBin) {
    $env:PATH = "$gitUsrBin;$env:PATH"
}

# CI may restore a partial cache from older /MT builds (restore-keys); force a clean link graph.
if ($CleanLlamaCache -or $env:CI -eq "true") {
    $llcb = Join-Path $env:LOCALAPPDATA "llcb"
    if (Test-Path $llcb) {
        Write-Host "Removing llama-cpp CMake cache: $llcb"
        Remove-Item -Recurse -Force $llcb
    }
    Write-Host "cargo clean -p esaxx-rs -p llama-cpp-sys-4 -p akasha-daemon"
    cargo clean -p esaxx-rs -p llama-cpp-sys-4 -p akasha-daemon
}

Write-Host "Building akasha-daemon (CUDA) for $Target ..."
cargo build --release --target $Target -p akasha-daemon --no-default-features --features $Features
