param([string]$CacheRoot = (Join-Path $env:LOCALAPPDATA 'AkashaDevTools'))
$ErrorActionPreference = 'Stop'
$version = '5.42.3.1'
$expected = '6A081A811781C30ACA51DBC036AFD93092AF91E3297901F02C17043795A10690'
$root = Join-Path $CacheRoot "strawberry-$version"
$archive = Join-Path $root 'perl.zip'
$perl = Join-Path $root 'perl/bin/perl.exe'
New-Item -ItemType Directory -Force -Path $root | Out-Null
if (!(Test-Path -LiteralPath $archive)) {
    Invoke-WebRequest -Uri "https://github.com/StrawberryPerl/Perl-Dist-Strawberry/releases/download/SP_54231_64bit/strawberry-perl-$version-64bit-portable.zip" -OutFile $archive
}
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) { throw 'Strawberry Perl archive checksum mismatch' }
if (!(Test-Path -LiteralPath $perl)) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [System.IO.Compression.ZipFile]::OpenRead($archive)
    try {
        $allowed = [System.IO.Path]::GetFullPath($root) + [System.IO.Path]::DirectorySeparatorChar
        foreach ($entry in $zip.Entries) {
            if (!$entry.FullName.StartsWith('perl/')) { continue }
            $target = [System.IO.Path]::GetFullPath((Join-Path $root $entry.FullName))
            if (!$target.StartsWith($allowed, [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Unsafe archive path' }
            if ($entry.FullName.EndsWith('/')) { New-Item -ItemType Directory -Force -Path $target | Out-Null; continue }
            New-Item -ItemType Directory -Force -Path ([System.IO.Path]::GetDirectoryName($target)) | Out-Null
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $true)
        }
    } finally { $zip.Dispose() }
}
# Process only; never change the user's PATH or persistent environment.
$env:PERL = $perl
$env:OPENSSL_SRC_PERL = $perl
if ($env:GITHUB_ENV) { "PERL=$perl" | Out-File $env:GITHUB_ENV -Append; "OPENSSL_SRC_PERL=$perl" | Out-File $env:GITHUB_ENV -Append }
& $perl -MLocale::Maketext::Simple -e 'print "SQLCipher build Perl ready\n"'
if ($LASTEXITCODE -ne 0) { throw 'Incomplete Perl runtime' }
