param([switch]$RecoverProfile,[string]$ProfileDir,[int]$Port=3877)
$ErrorActionPreference='Stop'
$package=Split-Path -Parent $PSScriptRoot
function Get-ComponentSha256([string]$Path) {
    $stream=[IO.File]::OpenRead($Path)
    $algorithm=[Security.Cryptography.SHA256]::Create()
    try { return [BitConverter]::ToString($algorithm.ComputeHash($stream)).Replace('-','').ToLowerInvariant() }
    finally { $stream.Dispose();$algorithm.Dispose() }
}
$manifest=Get-Content -LiteralPath (Join-Path $package 'consumer-package.json') -Raw | ConvertFrom-Json
if ($manifest.schema_version -ne 1 -or $manifest.edition -ne 'consumer-beta') { throw 'Paquet bêta incompatible. Conservez votre profil et téléchargez le paquet complet.' }
if (!$ProfileDir) { $ProfileDir=Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'AkashaBeta\profile' }
$profilePath=[IO.Path]::GetFullPath($ProfileDir)
if ($RecoverProfile) {
    try { $running=Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/privacy" -TimeoutSec 1 } catch { $running=$null }
    if ($running) { throw "Fermez la bêta avant de revenir au profil initial." }
    $pointer=Join-Path $profilePath 'consumer_active_profile.json'
    if (Test-Path -LiteralPath $pointer) {
        $saved=Join-Path $profilePath ('consumer_active_profile.recovery-'+[guid]::NewGuid().ToString()+'.json')
        Move-Item -LiteralPath $pointer -Destination $saved
        Write-Host 'Le profil initial sera utilisé au prochain lancement. Les profils restaurés et le réglage précédent sont conservés.'
    } else { Write-Host 'Aucun changement de profil à annuler.' }
    exit 0
}
# Verify immutable components before executing them. This never installs over C:\Akasha.
foreach ($file in $manifest.files) {
    $path=[IO.Path]::GetFullPath((Join-Path $package $file.path))
    $root=[IO.Path]::GetFullPath($package).TrimEnd('\')+'\'
    if (!$path.StartsWith($root,[StringComparison]::OrdinalIgnoreCase)) { throw 'Chemin de composant invalide.' }
    if (!(Test-Path -LiteralPath $path -PathType Leaf) -or (Get-ComponentSha256 $path) -ne $file.sha256) { throw "Composant incomplet ou modifié : $($file.path). Récupérez le paquet complet." }
}
try { $existing=Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/privacy" -TimeoutSec 2 } catch { $existing=$null }
if ($existing) { throw "Une instance utilise déjà le port $Port. Fermez cette instance avant de lancer la bêta ; elle n'a pas été arrêtée automatiquement." }
New-Item -ItemType Directory -Force -Path $profilePath | Out-Null
Remove-Item Env:AKASHA_PROFILE_BOOTSTRAP_DIR -ErrorAction SilentlyContinue
$env:AKASHA_DATA_DIR=$profilePath;$env:AKASHA_PORT=[string]$Port;$env:AKASHA_BIND='127.0.0.1';$env:AKASHA_SPEC_DIR=Join-Path $package 'spec'
$daemon=Join-Path $package 'akasha-daemon.exe'
function Start-OwnedDaemon {
    $process=Start-Process -FilePath $daemon -WorkingDirectory $package -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $profilePath 'consumer-startup.log')
    # Retain the process handle: Windows PowerShell 5.1 otherwise loses ExitCode
    # after an asynchronously started process exits.
    $null=$process.Handle
    return $process
}
$child=Start-OwnedDaemon
try {
    $deadline=[DateTime]::UtcNow.AddSeconds(90);$privacy=$null
    while ([DateTime]::UtcNow -lt $deadline) {
        if ($child.HasExited) { throw 'Akasha ne démarre pas. Conservez vos données ; RECOVER-PROFILE.cmd permet de revenir au profil initial sans effacer la restauration.' }
        try { $privacy=Invoke-RestMethod -Uri "http://127.0.0.1:$Port/api/privacy" -TimeoutSec 2;break } catch { Start-Sleep -Milliseconds 300 }
    }
    if (!$privacy.consumer_beta_build) { throw 'Le moteur protégé de la bêta ne répond pas. Aucune interface non protégée ne sera ouverte.' }
    $ui=Start-Process -FilePath (Join-Path $package 'AkashaBeta.exe') -WorkingDirectory $package -WindowStyle Normal -PassThru
    $null=$ui.Handle
    while (!$ui.HasExited) {
        if ($child.HasExited) {
            $child.WaitForExit()
            if ($child.ExitCode -eq 85) {
                $child=Start-OwnedDaemon
            } else { break }
        }
        Start-Sleep -Milliseconds 500
    }
} finally {
    # Stop only the process we started, after checking its absolute executable.
    if (!$child.HasExited) {
        $owned=Get-Process -Id $child.Id -ErrorAction SilentlyContinue
        if ($owned -and [IO.Path]::GetFullPath($owned.Path) -eq [IO.Path]::GetFullPath($daemon)) { Stop-Process -Id $child.Id }
    }
}
