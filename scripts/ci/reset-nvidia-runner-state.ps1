function Reset-NvidiaRunnerState {
    param([Parameter(Mandatory)][string]$Application)
    # JIT files are hidden on Windows; File.WriteAllBytes cannot replace them.
    # Call only after the listener exits. Never remove caches, work or profiles.
    foreach ($name in @('.runner', '.credentials', '.credentials_rsaparams')) {
        $file = Join-Path ([IO.Path]::GetFullPath($Application)) $name
        if (Test-Path -LiteralPath $file) { Remove-Item -LiteralPath $file -Force }
    }
}
