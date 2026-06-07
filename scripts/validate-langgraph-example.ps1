# Validate Python syntax of spec/dev/integrations/langgraph_memory_example.py
$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$ExamplePath = Join-Path $ProjectRoot "spec\dev\integrations\langgraph_memory_example.py"

if (-not (Test-Path -LiteralPath $ExamplePath)) {
    Write-Error "Missing file: $ExamplePath"
    exit 1
}

$python = Get-Command python -ErrorAction SilentlyContinue
if (-not $python) {
    $python = Get-Command python3 -ErrorAction SilentlyContinue
}
if (-not $python) {
    Write-Error "Python not found on PATH (python or python3)."
    exit 1
}

& $python.Source -m py_compile $ExamplePath
if ($LASTEXITCODE -ne 0) {
    Write-Error "Python syntax check failed for langgraph_memory_example.py"
    exit $LASTEXITCODE
}

Write-Host "OK: langgraph_memory_example.py syntax valid"
