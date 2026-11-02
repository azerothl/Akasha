param([string]$LlvmBin = (Join-Path $env:ProgramFiles 'LLVM/bin'))
$ErrorActionPreference = 'Stop'
$llvmRoot = [IO.Path]::GetFullPath($LlvmBin)
$clang = Join-Path $llvmRoot 'clang.exe'
$library = Join-Path $llvmRoot 'libclang.dll'
foreach ($component in @($clang, $library)) {
    if (!(Test-Path -LiteralPath $component -PathType Leaf)) { throw "Incomplete LLVM installation: $component" }
}
$resource = (& $clang --print-resource-dir).Trim()
if ($LASTEXITCODE -ne 0 -or !(Test-Path -LiteralPath (Join-Path $resource 'include/mmintrin.h'))) {
    throw 'The selected Clang has no matching intrinsic headers.'
}
# bindgen discovers its compiler independently of libclang. Visual Studio can
# put a different Clang first in PATH; keep the DLL, executable and builtins
# together without moving LLVM's link.exe ahead of the MSVC linker.
$env:LIBCLANG_PATH = $llvmRoot
$env:CLANG_PATH = $clang
$resourceArgument = '-resource-dir="' + $resource.Replace('\', '/') + '"'
$env:BINDGEN_EXTRA_CLANG_ARGS = (@($env:BINDGEN_EXTRA_CLANG_ARGS, $resourceArgument) | Where-Object { $_ }) -join ' '
if ($env:GITHUB_ENV) {
    foreach ($name in @('LIBCLANG_PATH', 'CLANG_PATH', 'BINDGEN_EXTRA_CLANG_ARGS')) {
        "$name=$([Environment]::GetEnvironmentVariable($name, 'Process'))" | Out-File -LiteralPath $env:GITHUB_ENV -Encoding utf8 -Append
    }
}
$version = (& $clang --version | Select-Object -First 1)
if ($LASTEXITCODE -ne 0) { throw 'The selected Clang executable did not run.' }
[pscustomobject]@{ clang = $clang; library = $library; resource_dir = $resource; version = $version }
