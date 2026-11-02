param([string]$ProfileDir, [ValidateRange(1,65535)][int]$Port=3876)
$ErrorActionPreference='Stop'
$package=[IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$manifest=Get-Content -LiteralPath (Join-Path $package 'release-beta-package.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if ($manifest.schema_version -ne 1 -or $manifest.edition -ne 'expert-beta') { throw 'Invalid Expert beta package.' }
$prefix=$package.TrimEnd('\')+'\'
foreach ($file in $manifest.files) {
    $path=[IO.Path]::GetFullPath((Join-Path $package $file.path))
    if (!$path.StartsWith($prefix,[StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid package path.' }
    $stream=[IO.File]::OpenRead($path);$algorithm=[Security.Cryptography.SHA256]::Create()
    try { $hash=[BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-','').ToLowerInvariant() }
    finally { $stream.Dispose();$algorithm.Dispose() }
    if ($hash -ne $file.sha256) { throw "Changed package component: $($file.path)" }
}
if (!$ProfileDir) { $ProfileDir=if ($env:AKASHA_DATA_DIR) {$env:AKASHA_DATA_DIR} else {Join-Path $env:USERPROFILE 'akasha'} }
if ($ProfileDir -eq '~') {$ProfileDir=$env:USERPROFILE}
elseif ($ProfileDir.StartsWith('~\') -or $ProfileDir.StartsWith('~/')) {$ProfileDir=Join-Path $env:USERPROFILE $ProfileDir.Substring(2)}
if ($ProfileDir -notmatch '^(?:[A-Za-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+(?:[\\/]|$))') {throw 'The data directory must be absolute.'}
$profilePath=[IO.Path]::GetFullPath($ProfileDir)
$probe=New-Object Net.Sockets.TcpClient
try { $busy=$probe.ConnectAsync('127.0.0.1',$Port).Wait(1000) -and $probe.Connected } catch {$busy=$false} finally {$probe.Dispose()}
if ($busy) {throw "Port $Port is already in use. Close the existing Akasha instance first. No process was stopped."}
New-Item -ItemType Directory -Force -Path $profilePath | Out-Null
$env:AKASHA_DATA_DIR=$profilePath;$env:AKASHA_PORT=[string]$Port;$env:AKASHA_BIND='127.0.0.1'
$env:AKASHA_SPEC_DIR=Join-Path $package 'spec'
# A prerelease must never stage a downgrade from the stable update channel.
$env:AKASHA_AUTO_UPDATE='0'
$daemon=Join-Path $package 'akasha-daemon.exe'
function Start-OwnedDaemon {
    $process=Start-Process -FilePath $daemon -WorkingDirectory $package -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $profilePath 'expert-beta-startup.log')
    $null=$process.Handle
    return $process
}
function Wait-OwnedDaemon($Process) {
    $deadline=[DateTime]::UtcNow.AddSeconds(90)
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($Process.HasExited) {throw 'The packaged daemon stopped during startup. Your data was retained.'}
        try {
            $privacy=Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/privacy" -TimeoutSec 2
            if ($privacy.process_id -eq $Process.Id -and !$privacy.consumer_beta_build) {return}
        } catch {}
        Start-Sleep -Milliseconds 300
    }
    throw 'The packaged Expert daemon did not become ready.'
}
$child=Start-OwnedDaemon
try {
    Wait-OwnedDaemon $child
    $ui=Start-Process -FilePath (Join-Path $package 'akasha-ui.exe') -WorkingDirectory $package -WindowStyle Normal -PassThru
    $null=$ui.Handle
    while (!$ui.HasExited) {
        if ($child.HasExited) {
            $child.WaitForExit()
            if ($child.ExitCode -eq 85) {$child=Start-OwnedDaemon;Wait-OwnedDaemon $child}
            else {throw 'The Expert daemon stopped. Your data was retained.'}
        }
        Start-Sleep -Milliseconds 500
    }
} finally {
    if (!$child.HasExited) {
        $live=Get-Process -Id $child.Id -ErrorAction SilentlyContinue
        if ($live -and $live.StartTime -eq $child.StartTime -and [IO.Path]::GetFullPath($live.Path) -eq $daemon) {Stop-Process -InputObject $live}
    }
}
