function Get-NvidiaWorkerPath {
    param([string]$CargoBin, [string]$GitExe, [string]$InheritedPath)
    $gitBin = Join-Path (Split-Path -Parent (Split-Path -Parent $GitExe)) 'bin'
    if (-not (Test-Path -LiteralPath (Join-Path $gitBin 'bash.exe'))) {
        throw 'Git for Windows with Git Bash is required; WSL bash cannot run Windows Actions scripts.'
    }
    return "$CargoBin;$gitBin;$InheritedPath"
}
