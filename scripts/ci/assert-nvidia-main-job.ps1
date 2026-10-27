# Installed outside the runner application directory. Executed before checkout.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'nvidia-main-policy.ps1')
    # Fail before contacting any external endpoint for a PR or foreign repository.
    Assert-NvidiaMainJob $env:GITHUB_REPOSITORY $env:GITHUB_REF $env:GITHUB_EVENT_NAME $env:GITHUB_SHA $env:GITHUB_SHA
    $request = [Net.Http.HttpClient]::new()
    try {
        $request.Timeout = [TimeSpan]::FromSeconds(20)
        $request.DefaultRequestHeaders.UserAgent.ParseAdd('Akasha-NVIDIA-runner')
        $request.DefaultRequestHeaders.Accept.ParseAdd('application/vnd.github+json')
        $body = $request.GetStringAsync('https://api.github.com/repos/azerothl/Akasha/git/ref/heads/main').GetAwaiter().GetResult()
        $main = ($body | ConvertFrom-Json).object.sha
    } finally { $request.Dispose() }
    Assert-NvidiaMainJob $env:GITHUB_REPOSITORY $env:GITHUB_REF $env:GITHUB_EVENT_NAME $env:GITHUB_SHA $main
    Write-Host "Authorized NVIDIA task on Akasha main: $main"
