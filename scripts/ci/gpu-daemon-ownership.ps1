function Test-GpuDaemonPortOwnership {
    param([int]$Port, [int]$ExpectedProcessId)
    $connection = [Net.Sockets.TcpClient]::new()
    try {
        if (-not $connection.ConnectAsync('127.0.0.1', $Port).Wait(1000)) { return $false }
        $clientPort = $connection.Client.LocalEndPoint.Port
        # Some Windows TCP providers omit a fresh listener from State=Listen.
        # An open connection gives us an exact server/client tuple to verify.
        $owners = @(Get-NetTCPConnection -LocalPort $Port -RemotePort $clientPort -ErrorAction SilentlyContinue)
        return [bool]($owners | Where-Object {
            $_.LocalAddress -eq '127.0.0.1' -and $_.OwningProcess -eq $ExpectedProcessId
        })
    } catch { return $false }
    finally { $connection.Dispose() }
}
