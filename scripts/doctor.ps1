# Run Akasha doctor (convenience script)
$ProjectRoot = Split-Path -Parent $PSScriptRoot
if (-not $ProjectRoot) { $ProjectRoot = (Get-Location).Path }
Set-Location $ProjectRoot

cargo run -p akasha-cli -- doctor $args
