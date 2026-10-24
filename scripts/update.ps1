# Windows PowerShell 5.1 compatible. Keep this file ASCII or UTF-8 with BOM.
param(
    [ValidateSet('Prepare', 'Install', 'Apply')][string]$Mode = 'Prepare',
    [string]$InstallDir = (Split-Path -Parent $PSScriptRoot),
    [string]$DataDir = $(if ($env:AKASHA_DATA_DIR) { $env:AKASHA_DATA_DIR } else { Join-Path $env:USERPROFILE 'akasha' }),
    [string]$ApiBase = '',
    [int]$WaitForPid = 0,
    [switch]$ConfirmInstall
)
$ErrorActionPreference = 'Stop'

function Convert-UpdateNativePath([string]$Path) {
    if ($Path.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) { return '\\' + $Path.Substring(8) }
    if ($Path.StartsWith('\\?\')) { return $Path.Substring(4) }
    return $Path
}

function Resolve-UpdatePath([string]$Root, [string]$Relative) {
    if (-not $Relative -or $Relative.Contains('\') -or $Relative.Contains(':') -or $Relative.StartsWith('/')) { throw "Invalid update path: $Relative" }
    foreach ($part in $Relative.Split('/')) {
        if (-not $part -or $part -in @('.', '..') -or $part -match '[<>"|?*\x00-\x1f]' -or $part -match '[. ]$' -or $part -match '^(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])(\.|$)') { throw "Invalid update path: $Relative" }
    }
    $target = [IO.Path]::GetFullPath((Join-Path $Root $Relative))
    $prefix = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    if (-not $target.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'Update path escapes installation' }
    $inspect = $target
    while ($inspect) {
        if (Test-Path -LiteralPath $inspect) {
            if ((Get-Item -LiteralPath $inspect -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Update path contains a link: $inspect" }
        }
        $inspect = Split-Path -Parent $inspect
    }
    return $target
}

function Assert-UpdateUrl([string]$Url) {
    $uri = [Uri]$Url
    if (-not $uri.IsAbsoluteUri -or $uri.Scheme -ne 'https' -or $uri.UserInfo) { throw 'Update URLs must use HTTPS without credentials' }
}

function Save-UpdateDownload([string]$Url, [string]$Destination, [long]$ExpectedSize = -1) {
    Add-Type -AssemblyName System.Net.Http
    $handler = New-Object Net.Http.HttpClientHandler
    $handler.AllowAutoRedirect = $false
    $client = New-Object Net.Http.HttpClient($handler)
    $client.Timeout = [TimeSpan]::FromMinutes(30)
    $client.DefaultRequestHeaders.UserAgent.ParseAdd('Akasha-Updater/1')
    $response = $null
    $stream = $null
    $output = $null
    try {
        for ($redirect = 0; $redirect -le 5; $redirect++) {
            Assert-UpdateUrl $Url
            $response = $client.GetAsync($Url, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
            if ([int]$response.StatusCode -in @(301, 302, 303, 307, 308)) {
                if (-not $response.Headers.Location -or $redirect -eq 5) { throw 'Invalid update redirect' }
                $Url = ([Uri]::new([Uri]$Url, $response.Headers.Location)).AbsoluteUri
                $response.Dispose()
                continue
            }
            $response.EnsureSuccessStatusCode() | Out-Null
            break
        }
        $limit = if ($ExpectedSize -ge 0) { $ExpectedSize } else { 8MB }
        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $output = [IO.File]::Create($Destination)
        $buffer = New-Object byte[] 65536
        [long]$total = 0
        while (($count = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            $total += $count
            if ($total -gt $limit) { throw 'Update download exceeds declared size' }
            $output.Write($buffer, 0, $count)
        }
        if ($ExpectedSize -ge 0 -and $total -ne $ExpectedSize) { throw 'Incomplete update download' }
    } finally {
        if ($output) { $output.Dispose() }
        if ($stream) { $stream.Dispose() }
        if ($response) { $response.Dispose() }
        $client.Dispose()
        $handler.Dispose()
    }
}

function Get-UpdateHash([string]$Path) {
    $algorithm = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try { return [BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $algorithm.Dispose() }
}

function Test-UpdateHash([string]$Path, [string]$Hash, [long]$Size) {
    return (Test-Path -LiteralPath $Path -PathType Leaf) -and (Get-Item -LiteralPath $Path).Length -eq $Size -and (Get-UpdateHash $Path) -eq $Hash
}

function Assert-UpdateManifest($Manifest, [string]$Target, [string]$Root) {
    if ($Manifest.schema_version -ne 1 -or $Manifest.target -cne $Target -or $Manifest.version -notmatch '^\d+\.\d+\.\d+$') { throw 'Incompatible update manifest' }
    if ($Target -notin @('windows-x86_64', 'windows-x86_64-cuda')) { throw 'Unsupported update target' }
    $seen = @{}
    if (@($Manifest.components).Count -lt 1 -or @($Manifest.components).Count -gt 100) { throw 'Invalid update components' }
    foreach ($component in $Manifest.components) {
        Assert-UpdateUrl $component.url
        if ($component.sha256 -notmatch '^[a-fA-F0-9]{64}$' -or $component.size -le 0 -or $component.size -gt 4GB -or @($component.files).Count -lt 1) { throw 'Invalid update component' }
        foreach ($file in $component.files) {
            $name = [string]$file.path
            Resolve-UpdatePath $Root $name | Out-Null
            if ($name -notmatch '^(akasha(-daemon|-tui)?\.exe|akasha-install\.json|cudart64_[^/]+\.dll|cublas(Lt)?64_[^/]+\.dll|scripts/(update|install|setup)\.ps1|(docs|spec|playwright-runner)/.+)$') { throw "Unmanaged update file: $name" }
            if ($seen.ContainsKey($name) -or $file.sha256 -notmatch '^[a-fA-F0-9]{64}$' -or $null -eq $file.size -or $file.size -lt 0 -or $file.size -gt 4GB) { throw 'Invalid or duplicate update file' }
            $seen[$name] = $true
            if ($seen.Count -gt 20000) { throw 'Too many update files' }
        }
    }
    foreach ($required in @('akasha.exe', 'akasha-daemon.exe', 'akasha-tui.exe', 'akasha-install.json', 'scripts/update.ps1')) {
        if (-not $seen.ContainsKey($required)) { throw "Incomplete manifest: $required" }
    }
}

function Expand-UpdateComponent([string]$Zip, $Component, [string]$Stage) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Zip)
    $expected = @{}
    foreach ($file in $Component.files) { $expected[[string]$file.path] = $file }
    $seen = @{}
    try {
        foreach ($entry in $archive.Entries) {
            $name = $entry.FullName
            if (-not $expected.ContainsKey($name) -or $seen.ContainsKey($name)) { throw "Unexpected ZIP entry: $name" }
            $seen[$name] = $true
            $file = $expected[$name]
            if ($entry.Length -ne $file.size -or (($entry.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) { throw 'Invalid ZIP file size or link' }
            $destination = Resolve-UpdatePath $Stage $name
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $destination, $true)
            if (-not (Test-UpdateHash $destination $file.sha256 $file.size)) { throw "SHA-256 mismatch: $name" }
        }
        if ($seen.Count -ne $expected.Count) { throw 'ZIP is missing update files' }
    } finally { $archive.Dispose() }
}

function Write-UpdateJson([string]$Path, $Value) {
    $temporary = $Path + '.tmp'
    Resolve-UpdatePath (Split-Path -Parent $Path) (Split-Path -Leaf $Path) | Out-Null
    Resolve-UpdatePath (Split-Path -Parent $temporary) (Split-Path -Leaf $temporary) | Out-Null
    [IO.File]::WriteAllText($temporary, ($Value | ConvertTo-Json -Depth 20), [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $Path -Force
}

function Set-UpdateStatus([string]$State, [string]$Version, [string]$ErrorText = '', [long]$Bytes = 0, [bool]$Recovery = $false) {
    Write-UpdateJson (Join-Path $script:Cache 'status.json') @{
        state = $State; remote_version = $Version; error = $ErrorText; download_bytes = $Bytes
        recovery_required = $Recovery
        updated_at = [DateTime]::UtcNow.ToString('o'); installed_version = $script:Marker.version
    }
}

function Prepare-AkashaUpdate {
    if (Test-Path -LiteralPath (Join-Path $script:Cache 'transaction.json')) {
        $plan = Get-Content -LiteralPath (Join-Path $script:Cache 'plan.json') -Raw -Encoding UTF8 | ConvertFrom-Json
        Assert-UpdateManifest $plan.manifest $script:Marker.target $InstallDir
        Set-UpdateStatus 'ready' $plan.manifest.version 'The previous installation was interrupted. Confirm installation to restore and retry.' 0 $true
        return $true
    }
    $manifestUrl = $script:Marker.manifest_url
    if ($ApiBase) {
        $index = Join-Path $script:Cache 'latest.partial'
        Save-UpdateDownload ($ApiBase.TrimEnd('/') + '/api/latest.json') $index
        $latest = Get-Content -LiteralPath $index -Raw -Encoding UTF8 | ConvertFrom-Json
        $manifestUrl = $latest.update_manifests.($script:Marker.target)
        if (-not $manifestUrl) { throw 'This update server does not publish component manifests' }
    }
    $manifestFile = Join-Path $script:Cache 'manifest.partial'
    Save-UpdateDownload $manifestUrl $manifestFile
    $manifest = Get-Content -LiteralPath $manifestFile -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-UpdateManifest $manifest $script:Marker.target $InstallDir
    if ([version]$manifest.version -le [version]$script:Marker.version) {
        Set-UpdateStatus 'up_to_date' $script:Marker.version
        return $false
    }
    Set-UpdateStatus 'downloading' $manifest.version
    $stage = Resolve-UpdatePath $script:Cache ('staging/' + $manifest.version)
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    $changed = @()
    [long]$bytes = 0
    foreach ($component in $manifest.components) {
        $different = @($component.files | Where-Object { -not (Test-UpdateHash (Resolve-UpdatePath $InstallDir $_.path) $_.sha256 $_.size) })
        if (-not $different.Count) { continue }
        $missing = @($different | Where-Object { -not (Test-UpdateHash (Resolve-UpdatePath $stage $_.path) $_.sha256 $_.size) })
        if ($missing.Count) {
            $zip = Resolve-UpdatePath $script:Cache ($component.sha256.ToLowerInvariant() + '.zip')
            if (-not (Test-UpdateHash $zip $component.sha256 $component.size)) {
                Save-UpdateDownload $component.url ($zip + '.partial') $component.size
                if (-not (Test-UpdateHash ($zip + '.partial') $component.sha256 $component.size)) { throw 'Update asset SHA-256 mismatch' }
                Move-Item -LiteralPath ($zip + '.partial') -Destination $zip -Force
                $bytes += $component.size
            }
            Expand-UpdateComponent $zip $component $stage
        }
        $changed += $different
    }
    $newMarker = Get-Content -LiteralPath (Resolve-UpdatePath $stage 'akasha-install.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($newMarker.version -cne $manifest.version -or $newMarker.target -cne $script:Marker.target -or $newMarker.manifest_url -cne $script:Marker.manifest_url) { throw 'Invalid target installation metadata' }
    $port = if ($env:AKASHA_PORT) { [int]$env:AKASHA_PORT } else { 3876 }
    Write-UpdateJson (Join-Path $script:Cache 'plan.json') @{ manifest = $manifest; base_version = $script:Marker.version; files = $changed; data_dir = $DataDir; port = $port }
    Set-UpdateStatus 'ready' $manifest.version '' $bytes
    return $true
}

function Get-AkashaProcesses {
    return @(Get-CimInstance Win32_Process | Where-Object {
        $_.ExecutablePath -and ((Split-Path -Parent $_.ExecutablePath) -eq $InstallDir) -and
        ($_.Name -eq 'akasha-daemon.exe' -or ($_.Name -eq 'akasha.exe' -and $_.CommandLine -match '(?:^"[^"]+"|^\S+)\s+start(?:\s|$)'))
    })
}

function Start-UpdatedDaemon {
    Start-Process -FilePath (Join-Path $InstallDir 'akasha.exe') -ArgumentList 'start' -WorkingDirectory $InstallDir -WindowStyle Hidden
    $port = if ($env:AKASHA_PORT) { [int]$env:AKASHA_PORT } else { 3876 }
    for ($attempt = 0; $attempt -lt 60; $attempt++) {
        Start-Sleep -Seconds 1
        try {
            $response = Invoke-WebRequest -UseBasicParsing -Uri "http://127.0.0.1:$port/api/status" -TimeoutSec 1
            if ($response.StatusCode -eq 200 -and @(Get-AkashaProcesses | Where-Object Name -eq 'akasha-daemon.exe').Count) { return }
        } catch { }
    }
    throw 'The updated daemon did not become ready'
}

function Stop-InstallationProcesses {
    $running = @(Get-AkashaProcesses)
    foreach ($process in ($running | Where-Object Name -eq 'akasha.exe')) { Stop-Process -Id $process.ProcessId -ErrorAction Stop }
    foreach ($process in @(Get-AkashaProcesses | Where-Object Name -eq 'akasha-daemon.exe')) { Stop-Process -Id $process.ProcessId -ErrorAction Stop }
}

function Restore-UpdateTransaction($Transaction) {
    Assert-UpdateManifest $Transaction.manifest $script:Marker.target $InstallDir
    if ($Transaction.base_version -notmatch '^\d+\.\d+\.\d+$' -or $script:Marker.version -notin @($Transaction.base_version, $Transaction.manifest.version)) { throw 'Installation changed since the interrupted update; preserve backups for manual recovery' }
    if ($Transaction.backup_id -notmatch '^[a-f0-9]{32}$') { throw 'Invalid rollback directory' }
    $allowed = @{}
    foreach ($component in $Transaction.manifest.components) { foreach ($file in $component.files) { $allowed[$file.path] = $true } }
    $entries = @($Transaction.entries)
    for ($i = $entries.Count - 1; $i -ge 0; $i--) {
        $entry = $entries[$i]
        if (-not $allowed.ContainsKey($entry.path)) { throw 'Invalid rollback file' }
        $destination = Resolve-UpdatePath $InstallDir $entry.path
        if ($entry.existed) {
            $backup = Resolve-UpdatePath $script:Cache ('backup/' + $Transaction.backup_id + '/' + $entry.path)
            if (-not (Test-UpdateHash $backup $entry.old_hash $entry.old_size)) { throw 'Rollback backup is damaged; keep the updater cache for recovery' }
            Copy-Item -LiteralPath $backup -Destination $destination -Force
        } elseif (Test-Path -LiteralPath $destination) { Remove-Item -LiteralPath $destination -Force }
        if (Test-Path -LiteralPath ($destination + '.update-new')) { Remove-Item -LiteralPath ($destination + '.update-new') -Force }
    }
    Remove-Item -LiteralPath (Join-Path $script:Cache 'transaction.json') -Force
    $script:Marker = Get-Content -LiteralPath (Join-Path $InstallDir 'akasha-install.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    $safeBackup = Resolve-UpdatePath $script:Cache ('backup/' + $Transaction.backup_id)
    if (Test-Path -LiteralPath $safeBackup) { Remove-Item -LiteralPath $safeBackup -Recurse -Force }
}

function Apply-AkashaUpdate {
    $journal = Join-Path $script:Cache 'transaction.json'
    if (Test-Path -LiteralPath $journal) {
        $interrupted = Get-Content -LiteralPath $journal -Raw -Encoding UTF8 | ConvertFrom-Json
        Stop-InstallationProcesses
        Restore-UpdateTransaction $interrupted
        if ($interrupted.was_running) {
            $env:AKASHA_DATA_DIR = $interrupted.data_dir
            $env:AKASHA_PORT = [string]$interrupted.port
            Start-UpdatedDaemon
        }
    }
    $plan = Get-Content -LiteralPath (Join-Path $script:Cache 'plan.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-UpdateManifest $plan.manifest $script:Marker.target $InstallDir
    if ($plan.base_version -cne $script:Marker.version) { throw 'Installation changed since update preparation; prepare again' }
    if (-not [IO.Path]::IsPathRooted($plan.data_dir) -or $plan.port -lt 1 -or $plan.port -gt 65535) { throw 'Invalid daemon restart context' }
    $env:AKASHA_DATA_DIR = $plan.data_dir
    $env:AKASHA_PORT = [string]$plan.port
    $stage = Resolve-UpdatePath $script:Cache ('staging/' + $plan.manifest.version)
    $allowed = @{}
    foreach ($component in $plan.manifest.components) { foreach ($file in $component.files) { $allowed[$file.path] = $file } }
    $seen = @{}
    foreach ($file in $plan.files) {
        if (-not $allowed.ContainsKey($file.path) -or $seen.ContainsKey($file.path) -or $allowed[$file.path].sha256 -cne $file.sha256 -or $allowed[$file.path].size -ne $file.size) { throw 'Invalid update plan' }
        $seen[$file.path] = $true
        if (-not (Test-UpdateHash (Resolve-UpdatePath $stage $file.path) $file.sha256 $file.size)) { throw 'Staged update is damaged; prepare again' }
    }
    if (-not $seen.ContainsKey('akasha-install.json')) { throw 'Update plan is missing installation metadata' }
    $backupId = [Guid]::NewGuid().ToString('N')
    $backup = Resolve-UpdatePath $script:Cache ('backup/' + $backupId)
    $transaction = @{ manifest = $plan.manifest; base_version = $plan.base_version; backup_id = $backupId; entries = @(); data_dir = $plan.data_dir; port = $plan.port }
    $running = @(Get-AkashaProcesses)
    $wasRunning = $running.Count -gt 0
    $transaction.was_running = $wasRunning
    Write-UpdateJson $journal $transaction
    Set-UpdateStatus 'applying' $plan.manifest.version
    try {
        # Only stop this installation, never every process named akasha-daemon.exe.
        Stop-InstallationProcesses
        foreach ($file in $plan.files) {
            $destination = Resolve-UpdatePath $InstallDir $file.path
            $old = Resolve-UpdatePath $backup $file.path
            $existed = Test-Path -LiteralPath $destination
            if ($existed) {
                New-Item -ItemType Directory -Force -Path (Split-Path -Parent $old) | Out-Null
                Copy-Item -LiteralPath $destination -Destination $old
            }
            New-Item -ItemType Directory -Force -Path (Split-Path -Parent $destination) | Out-Null
            $oldHash = if ($existed) { Get-UpdateHash $old } else { '' }
            $oldSize = if ($existed) { (Get-Item -LiteralPath $old).Length } else { 0 }
            $transaction.entries += @{ path = $file.path; existed = [bool]$existed; old_hash = $oldHash; old_size = $oldSize }
            # Persist the backup before replacing each file, including the version marker.
            Write-UpdateJson $journal $transaction
            $copied = $false
            for ($retry = 0; $retry -lt 20; $retry++) {
                try {
                    Copy-Item -LiteralPath (Resolve-UpdatePath $stage $file.path) -Destination ($destination + '.update-new') -Force
                    if (-not (Test-UpdateHash ($destination + '.update-new') $file.sha256 $file.size)) { throw 'Copied update file failed SHA-256 verification' }
                    Move-Item -LiteralPath ($destination + '.update-new') -Destination $destination -Force
                    $copied = $true
                    break
                } catch { Start-Sleep -Milliseconds 500 }
            }
            if (-not $copied) { throw "File still in use: $($file.path). Close Akasha terminals and retry." }
        }
        if ($wasRunning) { Start-UpdatedDaemon }
        $script:Marker.version = $plan.manifest.version
        Set-UpdateStatus 'installed' $plan.manifest.version
        Remove-Item -LiteralPath $journal -Force
    } catch {
        $failure = $_.Exception.Message
        Stop-InstallationProcesses
        Restore-UpdateTransaction $transaction
        if ($wasRunning) { Start-UpdatedDaemon }
        throw $failure
    }
    # All recursive cleanup targets are resolved under the dedicated updater cache.
    $safeBackup = Resolve-UpdatePath $script:Cache ('backup/' + (Split-Path -Leaf $backup))
    if (Test-Path -LiteralPath $safeBackup) { Remove-Item -LiteralPath $safeBackup -Recurse -Force }
    $safeStage = Resolve-UpdatePath $script:Cache ('staging/' + $plan.manifest.version)
    Remove-Item -LiteralPath $safeStage -Recurse -Force
    Get-ChildItem -LiteralPath $script:Cache -File | Where-Object Name -match '^[a-f0-9]{64}\.zip$' | Remove-Item -Force
}

# Dot sourcing exposes the parser/validation/download functions for isolated regression checks.
if ($MyInvocation.InvocationName -eq '.') { return }
$mutex = $null
$locked = $false
try {
    $InstallDir = [IO.Path]::GetFullPath((Convert-UpdateNativePath $InstallDir)).TrimEnd('\')
    $DataDir = [IO.Path]::GetFullPath((Convert-UpdateNativePath $DataDir))
    $env:AKASHA_DATA_DIR = $DataDir
    $script:Marker = Get-Content -LiteralPath (Resolve-UpdatePath $InstallDir 'akasha-install.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($script:Marker.schema_version -ne 1 -or $script:Marker.version -notmatch '^\d+\.\d+\.\d+$') { throw 'Invalid installation metadata' }
    $script:Cache = Resolve-UpdatePath $InstallDir '.akasha-update'
    New-Item -ItemType Directory -Force -Path $script:Cache | Out-Null
    if ($Mode -eq 'Apply' -and $WaitForPid -gt 0) {
        Wait-Process -Id $WaitForPid -Timeout 60 -ErrorAction SilentlyContinue
        if (Get-Process -Id $WaitForPid -ErrorAction SilentlyContinue) { throw 'The updating CLI has not exited' }
    }
    $hash = [Security.Cryptography.SHA256]::Create()
    $identity = [BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes($InstallDir.ToLowerInvariant()))).Replace('-', '')
    $hash.Dispose()
    $mutex = New-Object Threading.Mutex($false, ('Local\AkashaUpdate-' + $identity))
    try { $locked = $mutex.WaitOne(0) } catch [Threading.AbandonedMutexException] { $locked = $true }
    if (-not $locked) { throw 'Another update operation is already running' }
    if ($Mode -eq 'Apply') { Apply-AkashaUpdate }
    else {
        $ready = $false
        $statusPath = Join-Path $script:Cache 'status.json'
        $planPath = Join-Path $script:Cache 'plan.json'
        if ($Mode -eq 'Install' -and (Test-Path -LiteralPath $statusPath) -and (Test-Path -LiteralPath $planPath)) {
            $previous = Get-Content -LiteralPath $statusPath -Raw -Encoding UTF8 | ConvertFrom-Json
            $plan = Get-Content -LiteralPath $planPath -Raw -Encoding UTF8 | ConvertFrom-Json
            $ready = $previous.state -eq 'ready' -and $plan.base_version -ceq $script:Marker.version
        }
        if (-not $ready) { $ready = Prepare-AkashaUpdate }
        if ($Mode -eq 'Install' -and $ready) {
            if (-not $ConfirmInstall) {
                $answer = Read-Host 'Install the prepared update and restart the daemon if running? [y/N]'
                if ($answer -notmatch '^(y|yes|o|oui)$') { Write-Output 'Update kept ready for later.'; exit 0 }
            }
            $helper = Join-Path $script:Cache ('helper-' + [Guid]::NewGuid().ToString('N') + '.ps1')
            Copy-Item -LiteralPath $PSCommandPath -Destination $helper
            $parentPid = if ($WaitForPid -gt 0) { $WaitForPid } else { $PID }
            $arguments = @('-NoLogo', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $helper + '"'), '-Mode', 'Apply', '-InstallDir', ('"' + $InstallDir + '"'), '-DataDir', ('"' + $DataDir + '"'), '-WaitForPid', $parentPid)
            Start-Process -FilePath 'powershell.exe' -ArgumentList $arguments -WindowStyle Hidden
            Write-Output 'Update scheduled. Check akasha update check for the result.'
        } elseif (-not $ready) { Write-Output 'Akasha is up to date.' }
    }
} catch {
    $failureText = $_.Exception.Message
    if ($locked -and $script:Cache) {
        try {
            $lastStatus = Join-Path $script:Cache 'status.json'
            $previous = if (Test-Path -LiteralPath $lastStatus) { Get-Content -LiteralPath $lastStatus -Raw -Encoding UTF8 | ConvertFrom-Json } else { $null }
            if ($previous -and $previous.state -eq 'ready') {
                $previous.error = $failureText
                Write-UpdateJson $lastStatus $previous
            } else { Set-UpdateStatus 'failed' ([string]$previous.remote_version) $failureText }
        } catch { }
    }
    Write-Error $_ -ErrorAction Continue
    exit 1
} finally {
    if ($locked) { $mutex.ReleaseMutex() }
    if ($mutex) { $mutex.Dispose() }
}
