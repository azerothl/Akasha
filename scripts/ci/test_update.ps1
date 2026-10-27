# Isolated Windows PowerShell 5.1 updater regression: never runs installed executables.
param([Parameter(Mandatory = $true)][string]$Fixture)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$updater = Join-Path $root 'scripts\update.ps1'
$install = Join-Path $Fixture 'installed'
$userData = Join-Path $Fixture 'user-data'
New-Item -ItemType Directory -Path $userData | Out-Null
[IO.File]::WriteAllText((Join-Path $userData 'model.gguf'), 'user model sentinel')
. $updater -InstallDir $install -DataDir $userData
$script:Marker = Get-Content (Join-Path $install 'akasha-install.json') -Raw | ConvertFrom-Json
$script:Cache = Join-Path $install '.akasha-update'
New-Item -ItemType Directory -Force -Path $script:Cache | Out-Null
$requests = New-Object Collections.Generic.List[string]
$corruptDownload = $false
function Assert($Condition, [string]$Message) { if (-not $Condition) { throw $Message } }
function Assert-Throws([scriptblock]$Action, [string]$Message) {
    $threw = $false
    try { & $Action | Out-Null } catch { $threw = $true }
    Assert $threw $Message
}
function Save-UpdateDownload([string]$Url, [string]$Destination, [long]$ExpectedSize = -1) {
    $name = ([Uri]$Url).Segments[-1]
    $requests.Add($name)
    Copy-Item -LiteralPath (Join-Path $Fixture $name) -Destination $Destination -Force
    if ($corruptDownload -and $name -match '-akasha-daemon-') { [IO.File]::WriteAllText($Destination, 'invalid asset') }
}
function Get-AkashaProcesses { return @() }
function Start-UpdatedDaemon { throw 'No daemon should be launched by this isolated fixture' }
function Start-Sleep { param($Seconds, $Milliseconds) }

Assert ((Convert-UpdateNativePath '\\?\C:\Akasha') -eq 'C:\Akasha') 'Verbatim disk path not normalized for process matching'
Assert ((Convert-UpdateNativePath '\\?\UNC\server\share\Akasha') -eq '\\server\share\Akasha') 'Verbatim UNC path not normalized'
Assert ((Resolve-UpdateDataDir '~\akasha') -eq (Join-Path $env:USERPROFILE 'akasha')) 'Tilde data directory must resolve against the user profile'
$resolvedUserData=Resolve-UpdateDataDir $userData
# Windows PowerShell/.NET may expand the hosted runner's RUNNER~1 alias.
# Compare canonical paths and actual contents, rather than the spelling of TEMP.
Assert ($resolvedUserData -eq [IO.Path]::GetFullPath($userData)) 'Absolute data directory changed'
Assert ([IO.File]::ReadAllText((Join-Path $resolvedUserData 'model.gguf')) -eq 'user model sentinel') 'Resolved data directory lost its original contents'
Assert-Throws { Resolve-UpdateDataDir 'relative-data' } 'Relative data directory must not use updater working directory'
Assert-Throws { Resolve-UpdateDataDir '' } 'Empty data directory accepted'

foreach ($path in @('../outside', 'docs/../../outside', 'C:/outside', 'docs/a:stream', 'docs/CON', 'docs/trailing.')) {
    Assert-Throws { Resolve-UpdatePath $install $path } "Accepted unsafe path $path"
}
Assert-Throws { Assert-UpdateUrl 'http://example.com/update.zip' } 'Accepted an HTTP update'
$before = Get-UpdateHash (Join-Path $install 'akasha-daemon.exe')
$oldDaemonBytes = [IO.File]::ReadAllBytes((Join-Path $install 'akasha-daemon.exe'))
$oldMarkerBytes = [IO.File]::ReadAllBytes((Join-Path $install 'akasha-install.json'))
$corruptDownload = $true
Assert-Throws { Prepare-AkashaUpdate } 'Accepted an asset with the wrong SHA-256'
Assert ((Get-UpdateHash (Join-Path $install 'akasha-daemon.exe')) -eq $before) 'Corrupt asset modified installation'
$corruptDownload = $false
$requests.Clear()
Assert (Prepare-AkashaUpdate) 'No update prepared'
Assert ((Get-UpdateHash (Join-Path $install 'akasha-daemon.exe')) -eq $before) 'Preparation modified installed daemon'
Assert ($requests.Count -eq 3) ('Expected only manifest, daemon and resources requests: ' + ($requests -join ', '))
Assert (@($requests | Where-Object { $_ -match '-(cudart|akasha-tui)-' }).Count -eq 0) 'Downloaded unchanged CUDA/TUI'
$firstRequests = $requests.Count
Assert (Prepare-AkashaUpdate) 'Repeated prepare lost update'
Assert ($requests.Count -eq ($firstRequests + 1)) 'Repeated prepare downloaded staged assets again'

$manifest = Get-Content (Join-Path $Fixture 'akasha-update-windows-x86_64-cuda.json') -Raw | ConvertFrom-Json
$duplicate = $manifest | ConvertTo-Json -Depth 20 | ConvertFrom-Json
$duplicate.components[0].files += $duplicate.components[0].files[0]
Assert-Throws { Assert-UpdateManifest $duplicate 'windows-x86_64-cuda' $install } 'Accepted duplicate manifest file'
Assert-Throws { Assert-UpdateManifest $manifest 'windows-x86_64' $install } 'Accepted wrong CUDA variant'
Assert-Throws { Expand-UpdateComponent (Join-Path $Fixture 'evil.zip') $manifest.components[0] (Join-Path $script:Cache 'evil') } 'Accepted ZIP traversal'

# Run the actual entry point with a declined confirmation. A ready update works offline.
$startInfo = New-Object Diagnostics.ProcessStartInfo
$startInfo.FileName = 'powershell.exe'
$startInfo.Arguments = '-NoProfile -ExecutionPolicy Bypass -File "' + $updater + '" -Mode Install -InstallDir "' + $install + '" -DataDir "' + $userData + '"'
$startInfo.UseShellExecute = $false
$startInfo.RedirectStandardInput = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$process = [Diagnostics.Process]::Start($startInfo)
$process.StandardInput.WriteLine('n')
$process.StandardInput.Close()
$output = $process.StandardOutput.ReadToEnd()
$errorOutput = $process.StandardError.ReadToEnd()
$process.WaitForExit()
Assert ($process.ExitCode -eq 0 -and $output.Contains('kept ready')) ('Declined update failed: ' + $output + $errorOutput)
Assert ((Get-UpdateHash (Join-Path $install 'akasha-daemon.exe')) -eq $before) 'Declining installed the update'

$staged = Join-Path $script:Cache 'staging\1.1.0\akasha-daemon.exe'
$valid = [IO.File]::ReadAllBytes($staged)
[IO.File]::WriteAllText($staged, 'corrupt')
Assert-Throws { Apply-AkashaUpdate } 'Accepted damaged staging payload'
Assert ((Get-UpdateHash (Join-Path $install 'akasha-daemon.exe')) -eq $before) 'Damaged staging modified installation'
[IO.File]::WriteAllBytes($staged, $valid)

# Simulate failure after the daemon has been replaced, and verify rollback.
function Move-Item {
    [CmdletBinding()]param([string]$LiteralPath, [string]$Destination, [switch]$Force)
    if ([IO.Path]::GetFileName($Destination) -ieq 'akasha-install.json' -and [IO.Path]::GetFileName($LiteralPath) -ieq 'akasha-install.json.update-new') { throw 'Injected replacement failure' }
    Microsoft.PowerShell.Management\Move-Item @PSBoundParameters
}
Assert-Throws { Apply-AkashaUpdate } 'Injected replacement failure was ignored'
Assert ((Get-UpdateHash (Join-Path $install 'akasha-daemon.exe')) -eq $before) 'Rollback did not restore the daemon'
Assert ((Get-Content (Join-Path $install 'akasha-install.json') -Raw | ConvertFrom-Json).version -eq '1.0.0') 'Rollback changed installed version'
Remove-Item Function:\Move-Item

# A failed new-daemon health check must restore the files and restart the old daemon.
$script:restartAttempts = 0
function Get-AkashaProcesses { return @([pscustomobject]@{ Name = 'akasha-daemon.exe'; ProcessId = 2147483647 }) }
function Stop-Process { [CmdletBinding()]param([int]$Id) }
function Start-UpdatedDaemon { $script:restartAttempts++; if ($script:restartAttempts -eq 1) { throw 'Injected health failure' } }
Assert-Throws { Apply-AkashaUpdate } 'Failed health check was ignored'
Assert ($script:restartAttempts -eq 2) 'Old daemon was not restarted after health failure'
Assert ((Get-UpdateHash (Join-Path $install 'akasha-daemon.exe')) -eq $before) 'Health failure did not restore old daemon'
Remove-Item Function:\Stop-Process
function Get-AkashaProcesses { return @() }
function Start-UpdatedDaemon { throw 'No daemon should be launched by this isolated fixture' }

# Simulate a process/power interruption after files and marker were replaced.
$interruptedId = [Guid]::NewGuid().ToString('N')
$entries = @()
foreach ($name in @('akasha-daemon.exe', 'akasha-install.json')) {
    $destination = Join-Path $install $name
    $backup = Resolve-UpdatePath $script:Cache ('backup/' + $interruptedId + '/' + $name)
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $backup) | Out-Null
    Copy-Item -LiteralPath $destination -Destination $backup
    $entries += @{ path = $name; existed = $true; old_hash = (Get-UpdateHash $backup); old_size = (Get-Item $backup).Length }
    Copy-Item -LiteralPath (Join-Path $script:Cache ('staging/1.1.0/' + $name)) -Destination $destination -Force
}
Write-UpdateJson (Join-Path $script:Cache 'transaction.json') @{ manifest = $manifest; base_version = '1.0.0'; backup_id = $interruptedId; entries = $entries; was_running = $false; data_dir = $userData; port = 3876 }
$script:Marker = Get-Content (Join-Path $install 'akasha-install.json') -Raw | ConvertFrom-Json
$beforeRecoveryRequests = $requests.Count
Assert (Prepare-AkashaUpdate) 'Interrupted installation was not offered for recovery'
Assert ($requests.Count -eq $beforeRecoveryRequests) 'Interrupted installation downloaded before recovery'
Assert ((Get-Content (Join-Path $script:Cache 'status.json') -Raw | ConvertFrom-Json).recovery_required) 'Recovery status missing'
Apply-AkashaUpdate
Assert ((Get-Content (Join-Path $install 'akasha-install.json') -Raw | ConvertFrom-Json).version -eq '1.1.0') 'Update did not install target version'
Assert ((Get-Content (Join-Path $script:Cache 'status.json') -Raw | ConvertFrom-Json).state -eq 'installed') 'Update result not persisted'
Assert ((Get-Content (Join-Path $userData 'model.gguf') -Raw) -eq 'user model sentinel') 'Update modified user models'
Assert (-not (Test-Path -LiteralPath (Join-Path $script:Cache 'transaction.json'))) 'Completed transaction journal retained'
Assert (-not (Prepare-AkashaUpdate)) 'Same version is offered again'

# Exercise the real detached helper, parent wait and mutex using only fixture files.
[IO.File]::WriteAllBytes((Join-Path $install 'akasha-daemon.exe'), $oldDaemonBytes)
[IO.File]::WriteAllBytes((Join-Path $install 'akasha-install.json'), $oldMarkerBytes)
$script:Marker = Get-Content (Join-Path $install 'akasha-install.json') -Raw | ConvertFrom-Json
Assert (Prepare-AkashaUpdate) 'Could not prepare helper fixture'
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File $updater -Mode Install -InstallDir $install -DataDir $userData -ConfirmInstall
Assert ($LASTEXITCODE -eq 0) 'Could not launch update helper'
$completed = $false
for ($i = 0; $i -lt 60; $i++) {
    Microsoft.PowerShell.Utility\Start-Sleep -Milliseconds 250
    $status = Get-Content (Join-Path $script:Cache 'status.json') -Raw | ConvertFrom-Json
    if ($status.state -eq 'installed') { $completed = $true; break }
    if ($status.state -eq 'failed') { throw ('Detached helper failed: ' + $status.error) }
}
Assert $completed ('Detached helper did not finish: ' + ($status | ConvertTo-Json))
Assert ((Get-Content (Join-Path $install 'akasha-install.json') -Raw | ConvertFrom-Json).version -eq '1.1.0') 'Detached helper did not install target files'
Write-Output 'Windows updater regression checks passed'
