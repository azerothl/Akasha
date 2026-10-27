param(
    [string]$Root = 'C:\AkashaCI',
    [switch]$InstallStartup,
    [switch]$Worker
)
$ErrorActionPreference = 'Stop'
$runnerVersion = '2.338.2'
$runnerSha256 = 'a1f17aa987fc8158bc7521d284912267a0d01b7c2c4684b33c86783b5e8221cc'
$Root = [IO.Path]::GetFullPath($Root)
$application = Join-Path $Root "runner-$runnerVersion"
$hooks = Join-Path $Root 'hooks'
$logs = Join-Path $Root 'logs'
$manager = Join-Path $Root 'start-nvidia-runner.ps1'

if (-not $IsWindows) { throw 'This runner requires Windows x64 and an NVIDIA GPU.' }
if (-not $Worker) {
    foreach ($command in @('gh', 'git', 'pwsh', 'nvidia-smi', 'python', 'cmake')) {
        if (-not (Get-Command $command -ErrorAction SilentlyContinue)) { throw "Missing prerequisite: $command" }
    }
    if ((Get-PSDrive -Name ([IO.Path]::GetPathRoot($Root).Substring(0, 1))).Free -lt 35GB) {
        throw 'At least 35 GiB free is required for the CUDA build.'
    }
    foreach ($folder in @($Root, $application, $hooks, $logs, (Join-Path $Root 'work'), (Join-Path $Root 'temp'), (Join-Path $Root 'state'), (Join-Path $Root 'profile'), (Join-Path $Root 'state/roaming'))) {
        New-Item -ItemType Directory -Path $folder -Force | Out-Null
    }
    $archive = Join-Path $Root "actions-runner-win-x64-$runnerVersion.zip"
    if (-not (Test-Path $archive)) {
        Invoke-WebRequest "https://github.com/actions/runner/releases/download/v$runnerVersion/actions-runner-win-x64-$runnerVersion.zip" -OutFile $archive
    }
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLower() -ne $runnerSha256) { throw 'Runner archive checksum mismatch.' }
    if (-not (Test-Path (Join-Path $application 'bin/Runner.Listener.exe'))) {
        Expand-Archive -LiteralPath $archive -DestinationPath $application
    }
    foreach ($name in @('assert-nvidia-main-job.ps1', 'nvidia-main-policy.ps1', 'reset-nvidia-runner-state.ps1')) {
        Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $hooks $name) -Force
    }
    if ([IO.Path]::GetFullPath($PSCommandPath) -ne $manager) { Copy-Item -LiteralPath $PSCommandPath -Destination $manager -Force }
    if ($InstallStartup) {
        $shortcut = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path ([Environment]::GetFolderPath('Startup')) 'Akasha NVIDIA CI.lnk'))
        $shortcut.TargetPath = (Get-Command pwsh).Source
        $shortcut.Arguments = "-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$manager`" -Root `"$Root`" -Worker"
        $shortcut.WindowStyle = 7
        $shortcut.Save()
    }
    $process = Start-Process -FilePath (Get-Command pwsh).Source -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$manager`"", '-Root', "`"$Root`"", '-Worker') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $logs 'manager.stdout.log') -RedirectStandardError (Join-Path $logs 'manager.stderr.log')
    Write-Host "NVIDIA runner manager started (PID $($process.Id)). Logs: $logs"
    exit 0
}

$mutex = [Threading.Mutex]::new($false, 'Local\AkashaNvidiaRunner')
if (-not $mutex.WaitOne(0)) { Write-Host 'The NVIDIA runner manager is already active.'; exit 0 }
try {
    . (Join-Path $hooks 'reset-nvidia-runner-state.ps1')
    while (-not (Test-Path (Join-Path $Root 'stop'))) {
        try {
            $registered = gh api repos/azerothl/Akasha/actions/runners | ConvertFrom-Json
            if ($LASTEXITCODE) { throw 'Cannot inspect existing runners.' }
            foreach ($old in @($registered.runners | Where-Object { $_.name -eq 'akasha-nvidia-windows' })) {
                if ($old.status -ne 'offline' -or $old.busy -or 'akasha-nvidia-main' -notin $old.labels.name) {
                    throw 'An existing NVIDIA runner is still active; it will not be replaced.'
                }
                gh api --method DELETE "repos/azerothl/Akasha/actions/runners/$($old.id)" | Out-Null
                if ($LASTEXITCODE) { throw 'Cannot remove the offline runner registration.' }
            }
            $active = @(Get-CimInstance Win32_Process -Filter "Name='Runner.Listener.exe'" | Where-Object { $_.ExecutablePath -eq (Join-Path $application 'bin/Runner.Listener.exe') })
            if ($active.Count) { throw 'A listener is still running; its configuration will not be changed.' }
            Reset-NvidiaRunnerState -Application $application
            # The administrative token stays in this manager; it is never inherited by a job.
            $request = @{ name = 'akasha-nvidia-windows'; runner_group_id = 1; labels = @('self-hosted', 'Windows', 'X64', 'akasha-nvidia-main'); work_folder = (Join-Path $Root 'work') } | ConvertTo-Json -Compress
            $configuration = $request | gh api --method POST repos/azerothl/Akasha/actions/runners/generate-jitconfig --input - | ConvertFrom-Json
            if ($LASTEXITCODE -or -not $configuration.encoded_jit_config) { throw 'Cannot register a just-in-time runner.' }
            $start = [Diagnostics.ProcessStartInfo]::new((Join-Path $application 'bin/Runner.Listener.exe'))
            $start.WorkingDirectory = $application
            $start.UseShellExecute = $false
            $start.CreateNoWindow = $true
            $start.RedirectStandardOutput = $true
            $start.RedirectStandardError = $true
            foreach ($argument in @('run', '--jitconfig', $configuration.encoded_jit_config)) { $start.ArgumentList.Add($argument) }
            $start.Environment.Clear()
            foreach ($key in @('SystemRoot', 'WINDIR', 'ComSpec', 'PATH', 'PATHEXT', 'ProgramFiles', 'ProgramFiles(x86)', 'ProgramW6432', 'SystemDrive', 'OS', 'PROCESSOR_ARCHITECTURE', 'PROCESSOR_IDENTIFIER', 'NUMBER_OF_PROCESSORS', 'CUDA_PATH', 'LIBCLANG_PATH')) {
                $value = [Environment]::GetEnvironmentVariable($key)
                if ($value) { $start.Environment[$key] = $value }
            }
            $start.Environment['ACTIONS_RUNNER_HOOK_JOB_STARTED'] = Join-Path $hooks 'assert-nvidia-main-job.ps1'
            $start.Environment['CARGO_HOME'] = Join-Path $Root 'cargo'
            $start.Environment['CARGO_TARGET_DIR'] = Join-Path $Root 'target'
            $start.Environment['RUSTUP_HOME'] = Join-Path $Root 'rustup'
            $start.Environment['LOCALAPPDATA'] = Join-Path $Root 'state'
            $start.Environment['APPDATA'] = Join-Path $Root 'state/roaming'
            $start.Environment['USERPROFILE'] = Join-Path $Root 'profile'
            $start.Environment['HOME'] = Join-Path $Root 'profile'
            $start.Environment['TEMP'] = Join-Path $Root 'temp'
            $start.Environment['TMP'] = Join-Path $Root 'temp'
            $start.Environment['GH_CONFIG_DIR'] = Join-Path $Root 'state/gh'
            $start.Environment['GIT_CONFIG_GLOBAL'] = Join-Path $Root 'profile/gitconfig'
            $start.Environment['CARGO_BUILD_JOBS'] = '4'
            $start.Environment['CMAKE_BUILD_PARALLEL_LEVEL'] = '4'
            $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
            $process = [Diagnostics.Process]::Start($start)
            $stdout = $process.StandardOutput.ReadToEndAsync()
            $stderr = $process.StandardError.ReadToEndAsync()
            $process.WaitForExit()
            [IO.File]::WriteAllText((Join-Path $logs "runner-$stamp.stdout.log"), $stdout.GetAwaiter().GetResult())
            [IO.File]::WriteAllText((Join-Path $logs "runner-$stamp.stderr.log"), $stderr.GetAwaiter().GetResult())
            Write-Host "Runner $($configuration.runner.id) finished (exit $($process.ExitCode)); creating the next one-job runner."
        } catch { Write-Warning $_.Exception.Message }
        Start-Sleep -Seconds 15
    }
} finally { $mutex.ReleaseMutex(); $mutex.Dispose() }
