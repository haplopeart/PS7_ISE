<#
.SYNOPSIS
    Speaks the engine protocol to PowerShellEngineHost.exe by hand, and shows the replies.

.DESCRIPTION
    The debugger's answers are worth reading raw. The front end stitches them back together
    and paints them into a list, so when a pane shows nothing there are more reasons than there
    are bugs, and only two of them are in the front end.

    This starts the engine the way PS7_ISE.exe does, writes the records given in -Send to its
    standard input one after another, and prints what comes back verbatim, with the row and
    field marks spelled out as <RS> and <US> so a payload's shape can be read off the screen.

    Replies are read by a runspace of their own, because a debugger answer arrives while the
    next request is being written, and a reader that only looks between writes loses it.

.EXAMPLE
    .\harness\debug-protocol.ps1 -Send @('D l')

.EXAMPLE
    # A breakpoint on line 3, a script that reaches it, and the frame read while it is held.
    .\harness\debug-protocol.ps1 -Send @(
        'D b +3 C:\temp\dbg.ps1',
        ". 'C:\temp\dbg.ps1'",
        'D k', 'D v 0', 'D p $sum', 'D c'
    )
#>
param(
    # Where PowerShellEngineHost.exe lives.
    [string]$Engine = 'dist',
    # Protocol records, in order. One without a leading letter is typed at the prompt, so a
    # record can be written as `L . 'file.ps1'` or as plain `. 'file.ps1'`.
    [string[]]$Send = @('D l'),
    # The same thing from a file, one record per line: the records carry quotes and backslashes
    # that are miserable to pass through a shell, and a harness that cannot express the record
    # it is testing is not much use.
    [string]$SendFile = '',
    # Milliseconds to let the engine work between records.
    [int]$Wait = 2500,
    # Milliseconds to keep reading after the last record.
    [int]$Tail = 4000,
    # Every reply, verbatim and undivided, appended here. The console writer shortens long
    # records because nobody reads a six kilobyte line on screen, and a payload that has to be
    # counted or split into rows has to be read in full.
    [string]$OutFile = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$directory = Join-Path $root $Engine
$exe = Join-Path $directory 'PowerShellEngineHost.exe'
if (-not (Test-Path $exe)) { throw "No engine at $exe" }

$queue = [System.Collections.Concurrent.ConcurrentQueue[string]]::new()

$child = [System.Diagnostics.Process]::new()
$child.StartInfo.FileName = $exe
$child.StartInfo.WorkingDirectory = $directory
$child.StartInfo.UseShellExecute = $false
$child.StartInfo.RedirectStandardInput = $true
$child.StartInfo.RedirectStandardOutput = $true
$child.StartInfo.StandardOutputEncoding = [System.Text.UTF8Encoding]::new($false)
$child.StartInfo.CreateNoWindow = $true
[void]$child.Start()

# The reader has to run whether or not this script is doing anything else, so it gets a
# runspace rather than a ReadLine between writes.
$reader = [powershell]::Create().AddScript({
    param($stream, $out)
    while ($true) {
        $line = $stream.ReadLine()
        if ($null -eq $line) { break }
        $out.Enqueue($line)
    }
}).AddArgument($child.StandardOutput).AddArgument($queue)
$async = $reader.BeginInvoke()

function Show([string]$line) {
    $text = $line -replace [string][char]0x1E, '<RS>' -replace [string][char]0x1F, '<US>'
    if ($text.Length -gt 320) { $text = $text.Substring(0, 320) + " ... ($($line.Length) chars)" }
    Write-Host "  < $text" -ForegroundColor Cyan
}

function Drain([int]$milliseconds) {
    # The engine answers a busy session with a stream of module and verification noise, so
    # "nothing for this long" is the end of a reply, not "nothing for this many lines".
    $idle = 0
    while ($idle -lt $milliseconds) {
        $line = $null
        if ($queue.TryDequeue([ref]$line)) { Show $line; $idle = 0; continue }
        Start-Sleep -Milliseconds 100
        $idle += 100
    }
}

try {
    Write-Host "== engine pid $($child.Id) in $directory" -ForegroundColor Yellow
    Drain 3000

    $records = @($Send)
    if ($SendFile) {
        if (-not (Test-Path $SendFile)) { throw "No record file at $SendFile" }
        $records += Get-Content $SendFile | Where-Object { $_.Trim() -and -not $_.StartsWith('#') }
    }

    foreach ($record in $records) {
        $line = if ($record -match '^[A-Z] ') { $record } else { "L $record" }
        Write-Host "> $line" -ForegroundColor Green
        $child.StandardInput.WriteLine($line)
        $child.StandardInput.Flush()
        Drain $Wait
    }
    Drain $Tail
} finally {
    if (-not $child.HasExited) {
        try { $child.StandardInput.WriteLine('X'); $child.StandardInput.Flush() } catch { }
        if (-not $child.WaitForExit(3000)) { $child.Kill() }
    }
    $reader.Stop()
    $reader.Dispose()
    $child.Dispose()
}
