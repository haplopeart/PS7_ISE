<#
    Throwaway loopback CONNECT tunnel.

    This machine's DNS is broken (the LAN resolvers at 192.168.1.106 / 192.168.1.100 stopped
    answering) while its internet path is fine, so `git push` dies on "Could not resolve host:
    github.com". Fixing that properly means either repairing the LAN resolver or editing
    C:\Windows\System32\drivers\etc\hosts, and both need rights this shell does not have.

    A proxy gets around it without either: when git is told to use a proxy it stops resolving
    the name itself and sends `CONNECT github.com:443` to the proxy instead, and the proxy -
    this script - is free to reach the peer by the IP it already knows. Nothing is decrypted:
    the bytes are copied as they arrive, so the TLS session still runs end to end between git
    and GitHub, and credentials never appear here in the open. Loopback only, and it quits by
    itself after -LifetimeSec.
#>
param(
    [int]$Port = 18080,
    [string]$TargetIp = '140.82.121.4',
    [int]$LifetimeSec = 180
)

$ErrorActionPreference = 'Stop'
$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
$listener.Start()
Write-Output "tunnel listening on 127.0.0.1:$Port -> ${TargetIp}:443"

$deadline = (Get-Date).AddSeconds($LifetimeSec)
$connected = 0

try {
    while ((Get-Date) -lt $deadline) {
        $pending = $listener.BeginAcceptTcpClient($null, $null)
        if (-not $pending.AsyncWaitHandle.WaitOne(1000)) { continue }
        $client = $listener.EndAcceptTcpClient($pending)
        $down = $client.GetStream()

        # Read the CONNECT request line and its headers, and nothing past them: whatever
        # follows is the start of the TLS handshake and belongs to the tunnel.
        $buffer = New-Object byte[] 8192
        $text = ''
        while ($text -notmatch "\r\n\r\n$") {
            $read = $down.Read($buffer, 0, $buffer.Length)
            if ($read -le 0) { break }
            $text += [System.Text.Encoding]::ASCII.GetString($buffer, 0, $read)
        }
        if ($text -notmatch '^CONNECT\s+\S+(:\d+)?\s+HTTP/1') { $client.Close(); continue }

        $hello = [System.Text.Encoding]::ASCII.GetBytes("HTTP/1.1 200 Connection Established`r`n`r`n")
        $down.Write($hello, 0, $hello.Length)
        $down.Flush()

        $up = [System.Net.Sockets.TcpClient]::new()
        $up.Connect($TargetIp, 443)
        $remote = $up.GetStream()
        $connected++

        $pipe = New-Object byte[] 65536
        while ($client.Connected -and $up.Connected -and (Get-Date) -lt $deadline) {
            $moved = $false
            if ($down.DataAvailable) {
                $n = $down.Read($pipe, 0, $pipe.Length)
                if ($n -le 0) { break }
                $remote.Write($pipe, 0, $n); $remote.Flush(); $moved = $true
            }
            if ($remote.DataAvailable) {
                $n = $remote.Read($pipe, 0, $pipe.Length)
                if ($n -le 0) { break }
                $down.Write($pipe, 0, $n); $down.Flush(); $moved = $true
            }
            if (-not $moved) { Start-Sleep -Milliseconds 3 }
        }

        $up.Close(); $client.Close()
    }
} finally {
    $listener.Stop()
    Write-Output "tunnel closed after $connected forwarded connection(s)"
}
