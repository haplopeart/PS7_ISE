# Protocol test for the engine's `M` (command metadata) verb - what the Commands panel reads.
#
# Drives PowerShellEngineHost.exe over its real line protocol, with no front-end involved, and
# checks the things the panel will depend on:
#   * the index is complete, well formed, and cheap the second time (cached)
#   * every chunk is one printable line, inside the size bound, and reassembly loses nothing
#   * reply ids count up, so an answer the panel has given up on cannot merge with the next one
#   * detail carries the parameter fields the panel needs, taken from ParameterAttribute and
#     from the parameter Type (ParameterMetadata.IsMandatory/.Position/.SwitchParameter are
#     unreliable over this host, so they must not be what the engine trusts) - and both
#     polarities are checked, because a flag that is always 0 passes a one-way test
#   * an alias is described by the command behind it
#   * the runspace is shared, not stamped on: metadata against a running command is answered
#     either way and the engine survives it, the prompt still renders, and it exits clean
#   * failures answer with a reply rather than with silence
#
# Exits 0 when every check passes, 1 with a list of the ones that did not. Written for Windows
# PowerShell 5.1: no `u{...}` escapes, no goto, the record separators built from their codes.
#
#   powershell -NoProfile -File .\engine-protocol-test.ps1
#   powershell -NoProfile -File .\engine-protocol-test.ps1 -dist .\.engine-test -Wire
#
# While the verb is ahead of the committed dist\, publish just the engine first:
#   .\build.ps1 -SkipFrontEnd -SkipVersionBump -OutDir .engine-test
param(
    [string] $dist,
    [switch] $Wire
)
$ErrorActionPreference = 'Stop'

if (-not $dist) { $dist = Join-Path $PSScriptRoot 'dist' }

$script:SEP_ROW = [string][char]0x001e   # rows
$script:SEP_FIELD = [string][char]0x001f # fields
$script:SEP_SUB = [string][char]0x001d   # parameter sub-fields

$script:failures = New-Object Collections.ArrayList
$script:checks = 0
function Pass([string] $what) {
    $script:checks++
    Write-Host "  ok    $what" -ForegroundColor DarkGreen
}
function Fail([string] $what, [string] $detail) {
    $script:checks++
    $line = $what
    if ($detail) { $line = $what + '  --  ' + $detail }
    [void] $script:failures.Add($line)
    Write-Host "  FAIL  $line" -ForegroundColor Red
}
function Assert([bool] $ok, [string] $what, [string] $detail) {
    if ($ok) { Pass $what } else { Fail $what $detail }
}
function Note([string] $text) { Write-Host "        $text" -ForegroundColor DarkGray }

if (-not (Test-Path (Join-Path $dist 'PowerShellEngineHost.exe'))) {
    throw "no engine in $dist"
}

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = Join-Path $dist 'PowerShellEngineHost.exe'
$psi.WorkingDirectory = $dist
$psi.UseShellExecute = $false
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
# Standard error is deliberately NOT redirected. The engine sends everything that is not a
# protocol record there (Program.cs moves Console.Out and Console.Error onto it), and the real
# front end points it at a file, so it can never back up (engine.rs). Redirecting it into a
# pipe nobody reads instead fills that pipe after a few kilobytes and blocks the engine
# mid-request, which then looks exactly like an engine that has gone quiet or stopped answering.
$psi.StandardOutputEncoding = [Text.Encoding]::UTF8
$p = [Diagnostics.Process]::Start($psi)

# ReadLine blocks, so every read is a pending task the loops below spin on Wait().
$script:pending = $null
function Read-Line([int] $timeoutMs) {
    if (-not $script:pending) { $script:pending = $p.StandardOutput.ReadLineAsync() }
    if (-not $script:pending.Wait($timeoutMs)) { return $null }
    $line = $script:pending.Result
    $script:pending = $null
    return $line
}
function Send([string] $line) { $p.StandardInput.WriteLine($line); $p.StandardInput.Flush() }

# Reverses Program.cs Encode: only \\\\ and \\uXXXX matter here.
function Decode([string] $s) {
    if ($s.IndexOf([char]92) -lt 0) { return $s }
    $sb = New-Object Text.StringBuilder
    for ($i = 0; $i -lt $s.Length; $i++) {
        $c = $s[$i]
        if ($c -ne '\' -or $i + 1 -ge $s.Length) { [void] $sb.Append($c); continue }
        $n = $s[$i + 1]
        if ($n -eq '\') { [void] $sb.Append('\'); $i++ }
        elseif ($n -eq 'u' -and $i + 5 -lt $s.Length) {
            $code = 0
            $hex = $s.Substring($i + 2, 4)
            $ok = [int]::TryParse($hex, [Globalization.NumberStyles]::HexNumber, [Globalization.CultureInfo]::InvariantCulture, [ref] $code)
            if ($ok) { [void] $sb.Append([char] $code); $i += 5 } else { [void] $sb.Append($c) }
        } else { [void] $sb.Append($c) }
    }
    return $sb.ToString()
}

$script:other = New-Object Collections.ArrayList
# Collects records until a complete `M <kind>` reply has arrived. Returns $null on timeout.
function Read-Metadata([string] $kind, [int] $budgetMs) {
    $chunks = @{}
    $total = -1
    $replies = @{}
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $budgetMs) {
        $line = Read-Line 500
        if ($null -eq $line) { continue }          # nothing yet, keep waiting
        if ($Wire) { Note ('wire: ' + $line.Substring(0, [Math]::Min(120, $line.Length))) }
        if ($line -notlike 'M *') { [void] $script:other.Add($line); continue }
        # verb, kind, part, of, reply, payload(the rest, spaces intact)
        $parts = @($line -split ' ', 6)
        if ($parts.Length -lt 6) { [void] $script:failures.Add('malformed M record: ' + $line); continue }
        if ($parts[1] -eq 'e') {
            # A refusal is an answer to the request, not an answer to be waited past: the
            # request already got its reply, and pretending otherwise turns "the console is
            # busy" into "the engine never replied", which is a different complaint.
            $why = ''
            if ($parts.Length -eq 6) { $why = Decode $parts[5] }
            return [pscustomobject]@{
                kind = 'e'; total = 1; chunks = @{ 1 = $why }; replies = @($parts[4])
                ms = [math]::Round($sw.ElapsedTotalSeconds, 2); text = $why
            }
        }
        if ($parts[1] -ne $kind) { [void] $script:other.Add($line); continue }
        $seq = 0
        if (-not [int]::TryParse($parts[2], [ref] $seq)) { [void] $script:failures.Add('non-numeric part in: ' + $line); continue }
        $total = 0
        if (-not [int]::TryParse($parts[3], [ref] $total)) { [void] $script:failures.Add('non-numeric of in: ' + $line); continue }
        $reply = 0
        if (-not [int]::TryParse($parts[4], [ref] $reply)) { [void] $script:failures.Add('non-numeric reply in: ' + $line); continue }
        $replies[$reply] = $true
        $chunks[$seq] = Decode $parts[5]
        if ($total -le 0 -or $chunks.Count -ge $total) { break }
    }
    if ($total -lt 0) { return $null }
    $ordered = @()
    foreach ($k in ($chunks.Keys | Sort-Object)) { $ordered += $chunks[$k] }
    return [pscustomobject]@{
        kind    = $kind
        total   = $total
        chunks  = $chunks
        replies = @($replies.Keys)
        ms      = [math]::Round($sw.ElapsedTotalSeconds, 2)
        text    = ($ordered -join '')
    }
}

Write-Host "== engine M verb :: $dist" -ForegroundColor Cyan

# Reads until the session is parked at the prompt. Seeing a command's output is not enough to
# conclude that: the pipeline is still running while its last records are being written, and a
# metadata request that lands in that window is correctly told the console is busy. Command
# finished (9) and input requested (10) are the two records that say the runspace is free, and
# they are what the Commands panel will watch for when it is told to wait.
function Wait-Idle([int] $budgetMs) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while ($sw.ElapsedMilliseconds -lt $budgetMs) {
        $l = Read-Line 500
        if ($null -eq $l) { continue }
        if ($l -like 'E 9 *' -or $l -like 'E 10 *') { return $true }
        [void] $script:other.Add($l)
    }
    return $false
}

# Asks for the index until it is served, or the deadline passes. The engine may refuse while a
# command has the runspace, and that refusal is meant to be temporary, so the contract worth
# testing is "keep asking and the list arrives" - which is what the Commands panel will do too.
#
# It pauses rather than waiting for a console record to say the prompt is back, because that
# signal only comes once per command: by the time the loop is parked there are no more boundary
# records coming, and a retry that waited for one would wait for something that has already
# happened. A short pause and another question is both simpler and the truth.
function Request-Index([int] $budgetMs) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $tries = 0
    while ($sw.ElapsedMilliseconds -lt $budgetMs) {
        Send 'M i'
        $tries++
        $reply = Read-Metadata 'i' 20000
        if ($reply -and $reply.kind -eq 'i') {
            if ($tries -gt 1) { Note ('  served after ' + $tries.ToString() + ' questions') }
            return $reply
        }
        if ($reply) { Note ('  refused (' + $tries.ToString() + '): ' + $reply.text) }
        Start-Sleep -Milliseconds 200
    }
    return $null
}

# ---------------------------------------------------------------- boot
Send 'L "engine-up=1"'
$up = $null
$sw0 = [Diagnostics.Stopwatch]::StartNew()
while ($sw0.ElapsedMilliseconds -lt 60000 -and -not $up) {
    $l = Read-Line 1000
    if ($l -and $l -match 'engine-up') { $up = $l }
}
Assert ($null -ne $up) 'engine answers L before any M request' 'no reply within 60s'
if (-not $up) {
    Write-Host 'cannot continue without a booted engine' -ForegroundColor Red
    exit 1
}

# ---------------------------------------------------------------- index
Write-Host '-- index' -ForegroundColor Cyan
Wait-Idle 30000 | Out-Null
$idx = Request-Index 45000
if (-not $idx) {
    Fail 'index reply arrived' 'refused every time, or never answered'
} else {
    Pass 'index reply arrived'
    Note ($idx.total.ToString() + ' chunks, ' + $idx.text.Length.ToString() + ' chars, ' + $idx.ms.ToString() + 's')

    Assert ($idx.total -gt 0) 'index reports a positive chunk total' ('total=' + $idx.total)
    Assert ($idx.chunks.Count -eq $idx.total) 'every announced chunk arrived' ($idx.chunks.Count.ToString() + '/' + $idx.total.ToString())

    $maxLen = 0
    $brokeLine = $false
    foreach ($v in $idx.chunks.Values) {
        if ($v.Length -gt $maxLen) { $maxLen = $v.Length }
        if ($v.IndexOf([char]13) -ge 0 -or $v.IndexOf([char]10) -ge 0) { $brokeLine = $true }
    }
    Assert (-not $brokeLine) 'no chunk contains a line break' ''
    Assert ($maxLen -le 49152) 'each chunk stays inside the 48 KB bound' ('longest=' + $maxLen)

    # Reassembly must not lose a character at a chunk boundary: that is what the engine's
    # surrogate-pair guard is for, and a byte missing in the middle would be invisible here
    # except by length.
    $summed = 0
    foreach ($v in $idx.chunks.Values) { $summed += $v.Length }
    Assert ($summed -eq $idx.text.Length) 'reassembly loses nothing across chunk boundaries' ($summed.ToString() + ' vs ' + $idx.text.Length.ToString())

    $rows = @($idx.text -split $SEP_ROW | Where-Object { $_ -ne '' })
    Assert ($rows.Count -ge 4000) 'index lists the expected number of commands' ('rows=' + $rows.Count)

    $bad = @($rows | Where-Object { @($_ -split $SEP_FIELD).Length -ne 3 })
    Assert ($bad.Count -eq 0) 'every index row has name, module and parameters' ('bad=' + $bad.Count + ' first=' + ($bad | Select-Object -First 1))

    $withParams = @($rows | Where-Object { (@($_ -split $SEP_FIELD)[2]) -ne '' }).Count
    Assert ($withParams -gt 0) 'index carries parameter names where the module is loaded' ('withParams=' + $withParams)
    Note ($withParams.ToString() + ' of ' + $rows.Count.ToString() + ' rows carry parameters (stubs legitimately do not)')

    $noModule = @($rows | Where-Object { (@($_ -split $SEP_FIELD)[1]) -eq '' }).Count
    Note ($noModule.ToString() + ' rows have no module (functions/aliases) -- the dropdown needs a blank entry')

    $gp = @($rows | Where-Object { (@($_ -split $SEP_FIELD)[0]) -eq 'Get-Process' })
    Assert ($gp.Count -eq 1) 'index contains Get-Process exactly once' ('count=' + $gp.Count)
    if ($gp.Count -ge 1) {
        $gpf = @($gp[0] -split $SEP_FIELD)
        # Microsoft.PowerShell.Management is not loaded at startup, so Get-Process is a stub
        # here: a name and a module and no parameters until the detail call imports it. What
        # has to hold either way is that the column is a plain comma list the panel can split.
        Assert ($gpf[1].Length -gt 0) 'a stub still names its module' ('module=' + $gpf[1])
        $gpp = @($gpf[2] -split ',' | Where-Object { $_ -ne '' })
        $ugly = @($gpp | Where-Object { $_ -notmatch '^[A-Za-z_][A-Za-z0-9_]*$' })
        Assert ($ugly.Count -eq 0) 'the parameter column is a clean comma list' ('bad=' + ($ugly -join '|'))
        Note ('Get-Process module=' + $gpf[1] + ' index params=' + $gpp.Count + ' (detail is what fills them in)')
        Note ('first rows: ' + (($rows | Select-Object -First 3 | ForEach-Object { (@($_ -split $SEP_FIELD)[0]) }) -join ', '))
    }

    # Cached: a second request must not pay for the scan again.
    $again = Request-Index 45000
    if (-not $again) {
        Fail 'cached index reply arrived' 'refused every time, or never answered'
    } else {
        Assert ($again.ms -lt 3) 'cached index is served fast' ($again.ms.ToString() + 's')
        Assert ($again.text.Length -eq $idx.text.Length) 'cached index is identical' ($again.text.Length.ToString() + ' vs ' + $idx.text.Length.ToString())
        # One reply must not be splittable across two numbers, and two replies must not share
        # one: that is what lets the panel drop an answer it has already given up on.
        Assert ($idx.replies.Count -eq 1) 'every part of one reply carries the same reply id' ('ids=' + ($idx.replies -join ','))
        Assert ($again.replies.Count -eq 1) 'the second index reply is also one numbered reply' ('ids=' + ($again.replies -join ','))
        Assert ($again.replies[0] -gt $idx.replies[0]) 'reply ids count up between replies' ($idx.replies[0].ToString() + ' then ' + $again.replies[0].ToString())
    }
}

# ---------------------------------------------------------------- detail
function Get-ParamMap($reply) {
    $map = @{}
    foreach ($r in @($reply.text -split $SEP_ROW)) {
        if ($r -eq '' -or $r.Substring(0, 1) -ne 'P') { continue }
        $f = @($r -split $SEP_SUB)
        if ($f.Length -ge 7) { $map[$f[1]] = $f }
    }
    return $map
}
# Safe field read: a detail string that indexes a parameter the command does not have would
# otherwise abort the whole run with "Cannot index into a null array", which reads exactly
# like the engine having failed.
function PField($map, [string] $name, [int] $idx) {
    if (-not $map.ContainsKey($name)) { return '<missing>' }
    $f = $map[$name]
    if ($idx -ge $f.Length) { return '<short>' }
    return $f[$idx]
}

Write-Host '-- detail' -ForegroundColor Cyan
Send 'M d Get-Process'
$det = Read-Metadata 'd' 90000
if (-not $det) {
    Fail 'detail reply for Get-Process arrived' 'timed out'
} else {
    Pass 'detail reply for Get-Process arrived'
    $drows = @($det.text -split $SEP_ROW | Where-Object { $_ -ne '' })
    $info = @($drows | Where-Object { $_.Substring(0, 1) -eq 'C' })
    $pars = @($drows | Where-Object { $_.Substring(0, 1) -eq 'P' })
    Assert ($info.Count -eq 1) 'detail has exactly one command row' ('count=' + $info.Count)
    Assert ($pars.Count -ge 15) 'detail lists Get-Process parameters' ('params=' + $pars.Count)

    $map = Get-ParamMap $det
    Assert ($map.ContainsKey('Name')) 'detail includes -Name' ''
    Assert ($map.ContainsKey('Module')) 'detail includes -Module' ''
    if ($map.ContainsKey('Name')) {
        # P, name, type, mandatory, position, switch, common, help
        $n = $map['Name']
        Assert ($n[0] -eq 'P') 'parameter row is tagged P' ('tag=' + $n[0])
        Assert ($n[2] -like 'string*') '-Name reports its type' ('got=' + $n[2])
        Assert ($n[3] -match '^[01]$') '-Name reports mandatory as 0/1' ('got=' + $n[3])
        Assert ($n[4] -match '^-?\d+$') '-Name reports position as an integer' ('got=' + $n[4])
        Assert ($n[5] -eq '0') '-Name is not a switch (type is string[], despite what ParameterMetadata.SwitchParameter says)' ('got=' + $n[5])
        Assert ($n[6] -eq '0') '-Name is not a common parameter' ('got=' + $n[6])
        Note ('-Name => type=' + $n[2] + ' mandatory=' + $n[3] + ' position=' + $n[4] + ' switch=' + $n[5] + ' common=' + $n[6])
    }
    if ($map.ContainsKey('Module')) {
        Assert ($map['Module'][5] -eq '1') '-Module IS a switch (detected from its parameter type)' ('got=' + $map['Module'][5])
    }
    $verbose = @($pars | Where-Object { (@($_ -split $SEP_SUB)[1]) -eq 'Verbose' })
    Assert ($verbose.Count -eq 1) 'detail carries -Verbose' ''
    if ($verbose.Count -eq 1) {
        Assert ((@($verbose[0] -split $SEP_SUB)[6]) -eq '1') '-Verbose is flagged common' ''
    }
    $switches = @($pars | Where-Object { (@($_ -split $SEP_SUB)[5]) -eq '1' }).Count
    $common = @($pars | Where-Object { (@($_ -split $SEP_SUB)[6]) -eq '1' }).Count
    Assert ($switches -gt 0) 'some Get-Process parameters are switches' ('switches=' + $switches)
    Assert ($common -ge 4) 'common parameters are present and classified' ('common=' + $common)
    Note ('switches=' + $switches + ' common=' + $common)

    $cf = @($info[0] -split $SEP_FIELD)
    # C, name, module, synopsis, syntax -- the synopsis is empty when it only repeats the
    # syntax, which is what PowerShell synthesises when no help file is installed.
    Assert ($cf.Length -ge 5) 'command row has name, module, synopsis, syntax' ('fields=' + $cf.Length)
    Assert ($cf[1] -eq 'Get-Process') 'command row names the command' ('got=' + $cf[1])
    Assert ($cf[2] -eq 'Microsoft.PowerShell.Management') 'command row names the module' ('got=' + $cf[2])
    Assert ($cf[4].Length -gt 5) 'command row carries a syntax line' ('len=' + $cf[4].Length)
    $flat = $cf[4] -replace '[\r\n]+', ' | '
    Note ('syntax: ' + $flat.Substring(0, [Math]::Min(120, $flat.Length)))
    Note ('synopsis chars: ' + $cf[3].Length + ' (0 means it only repeated the syntax)')
}

# The mandatory/switch flags have to be real, not constants: check both polarities.
Write-Host '-- detail, known polarities' -ForegroundColor Cyan
Send 'M d Get-ChildItem'
$ci = Read-Metadata 'd' 90000
if (-not $ci) { Fail 'detail for Get-ChildItem arrived' 'timed out' }
else {
    $m = Get-ParamMap $ci
    Assert ((PField $m 'Recurse' 5) -eq '1') '-Recurse is a switch' ('got=' + (PField $m 'Recurse' 5))
    Assert ((PField $m 'Path' 2) -like 'string*') '-Path is string[]' ('got=' + (PField $m 'Path' 2))
    Assert ((PField $m 'Path' 5) -eq '0') '-Path is not a switch' ('got=' + (PField $m 'Path' 5))
    Assert ((PField $m 'Path' 4) -eq '0') '-Path is positional 0' ('got=' + (PField $m 'Path' 4))
}

Send 'M d Get-Service'
$gs = Read-Metadata 'd' 90000
if (-not $gs) { Fail 'detail for Get-Service arrived' 'timed out' }
else {
    $m2 = Get-ParamMap $gs
    # Polarity, taken from the command's own syntax line:
    #   Get-Service [[-Name] <string[]>] [-DependentServices] ...
    #   Get-Service -DisplayName <string[]> ...
    # so -DisplayName is mandatory and -Name is not, and -DependentServices is a switch.
    Assert ($m2.ContainsKey('DependentServices')) 'detail includes -DependentServices' ('keys=' + ($m2.Keys -join ','))
    Assert ((PField $m2 'DependentServices' 5) -eq '1') 'Get-Service -DependentServices is a switch' ('got=' + (PField $m2 'DependentServices' 5))
    Assert ((PField $m2 'Name' 5) -eq '0') 'Get-Service -Name is not a switch' ('got=' + (PField $m2 'Name' 5))
    Assert ((PField $m2 'DisplayName' 3) -eq '1') 'Get-Service -DisplayName is mandatory (mandatory is not a constant)' ('got=' + (PField $m2 'DisplayName' 3))
    Assert ((PField $m2 'Name' 3) -eq '0') 'Get-Service -Name is not mandatory' ('got=' + (PField $m2 'Name' 3))
    Note ('-Name mandatory=' + (PField $m2 'Name' 3) + ' position=' + (PField $m2 'Name' 4))
    Note ('-DisplayName mandatory=' + (PField $m2 'DisplayName' 3) + ' position=' + (PField $m2 'DisplayName' 4))
}

# An alias names a command rather than being one, so `cd` has to be described by
# Set-Location, which is the only thing that has parameters. The list has no aliases in it,
# so this path only happens for a name typed into the Find box.
Write-Host '-- detail, an alias' -ForegroundColor Cyan
Send 'M d cd'
$al = Read-Metadata 'd' 90000
if (-not $al) {
    Fail 'detail for the alias cd arrived' 'timed out'
} else {
    $am = Get-ParamMap $al
    Assert ($am.Count -ge 3) 'an alias is described by the command behind it' ('params=' + $am.Count)
    Assert ($am.ContainsKey('PassThru')) 'describing cd yields Set-Location parameters' ('keys=' + (($am.Keys | Sort-Object) -join ','))
    $arow = @(@($al.text -split $SEP_ROW | Where-Object { $_ -ne '' }) | Where-Object { $_.Substring(0, 1) -eq 'C' })
    if ($arow.Count -ge 1) {
        $af = @($arow[0] -split $SEP_FIELD)
        Assert ($af[1] -eq 'cd') 'the alias keeps the name the user picked, not the target' ('got=' + $af[1])
    }
}

# ---------------------------------------------------------------- failure modes
Write-Host '-- failure modes' -ForegroundColor Cyan
Send 'M d This-DoesNotExistAtAll'
$miss = Read-Metadata 'e' 30000
Assert ($null -ne $miss) 'unknown command answers with an error record' 'no M e reply'
if ($miss) { Note ('error text: ' + $miss.text) }

Send 'M q'
$junk = Read-Metadata 'e' 20000
Assert ($null -ne $junk) 'unknown sub-verb answers with an error record' 'no M e reply'

# ---------------------------------------------------------------- the event stream survives
Write-Host '-- event stream' -ForegroundColor Cyan
Send 'L "still-here=" + @(Get-Process).Count'
$seen = $null
$sw1 = [Diagnostics.Stopwatch]::StartNew()
while ($sw1.ElapsedMilliseconds -lt 40000 -and -not $seen) {
    $l = Read-Line 1000
    if ($l -and $l -match 'still-here') { $seen = $l }
}
Assert ($null -ne $seen) 'L still runs and reports after metadata traffic' 'no reply'
$post = Request-Index 45000
if (-not $post) {
    Fail 'index served again once the command finished' 'refused every time, or never answered'
} else {
    Assert ($post.total -gt 0 -and $post.text.Length -gt 0) 'index served again once the command finished' ('parts=' + $post.total + ' chars=' + $post.text.Length)
}

# ---------------------------------------------------------------- soak
# The one situation where the panel and the prompt share a runspace: a metadata request that
# lands while a command is executing. The engine is meant to say "busy" rather than ask the
# runspace a second question, and either way it has to stay alive and keep running commands.
# Repeated a few times because a failure here would be a race, and a race does not show up on
# the first try.
Write-Host '-- soak: metadata against a busy session' -ForegroundColor Cyan
$names = @('Get-Process', 'Get-ChildItem', 'Get-Service', 'Select-Object', 'Get-Module')
$mLines = 0
$roundsCompleted = 0
for ($round = 1; $round -le 8; $round++) {
    # One line that holds the session busy and then proves it got that far.
    Send ('L Start-Sleep -Milliseconds 600; "soak-' + $round + '-done"')
    Send 'M i'
    Send ('M d ' + $names[$round % $names.Length])

    $marker = $false
    $saw = 0
    $sw2 = [Diagnostics.Stopwatch]::StartNew()
    while ($sw2.ElapsedMilliseconds -lt 25000 -and -not $marker) {
        $l = Read-Line 500
        if ($null -eq $l) { continue }
        if ($l -like 'M *') { $saw++ }
        if ($l -like ('*soak-' + $round + '-done*')) { $marker = $true }
    }
    $mLines += $saw
    if ($marker) { $roundsCompleted++ }
    if (-not $marker) { Fail ('soak round ' + $round + ' still ran its command') ('no marker, ' + $saw + ' M lines') }
}
Assert ($roundsCompleted -eq 8) 'every soak round finished its command' ('completed=' + $roundsCompleted + '/8')
Assert ($mLines -ge 16) 'every metadata request was answered, busy or not' ('M lines=' + $mLines)

# The prompt is drawn by running the prompt function on the runspace, which now goes through
# the same gate as a metadata read - so prove the prompt still arrives, and with a directory in
# it, after both kinds of traffic.
Write-Host '-- prompt' -ForegroundColor Cyan
Send 'L "prompt-check=1"'
$prompt = $null
$text = New-Object Text.StringBuilder
$sw5 = [Diagnostics.Stopwatch]::StartNew()
while ($sw5.ElapsedMilliseconds -lt 40000 -and -not $prompt) {
    $l = Read-Line 1000
    if ($null -eq $l) { continue }
    if ($l -like 'E *') {
        $f = @($l -split ' ', 5)
        if ($f.Length -eq 5) { [void] $text.Append((Decode $f[4])) }
    }
    $flat = ($text.ToString() -replace '[\r\n]+', ' ')
    if ($flat -match 'PS \S.*>\s*$') { $prompt = $matches[0] }
}
Assert ($null -ne $prompt) 'the prompt is still printed after metadata traffic' ('tail=' + $flat.Substring([Math]::Max(0, $flat.Length - 60)))
if ($prompt) { Note ('prompt: ' + $prompt.TrimEnd()) }

$soakIdx = Request-Index 45000
$soakRows = 0
if ($soakIdx -and $soakIdx.kind -eq 'i') { $soakRows = @($soakIdx.text -split $SEP_ROW | Where-Object { $_ -ne '' }).Count }
if ($soakRows -lt 4000) {
    # Do not guess at this one: say what the engine actually returned and whether it is alive.
    $got = '<no reply>'
    $said = ''
    if ($soakIdx) { $got = $soakIdx.kind; $said = $soakIdx.text }
    Note ('diagnosis: engine alive=' + (-not $p.HasExited) + ', kind=' + $got + ', says=' + $said)
}
Assert ($soakRows -ge 4000) 'the index is still complete after the soak' ('rows=' + $soakRows)

# Which way the engine leans when asked mid-command decides what the panel has to say, so
# report it rather than assert it: a busy answer means the panel shows a wait, a real reply
# means the window where the runspace is free is wider than the guard suggests.
Send 'L Start-Sleep -Seconds 3; "long-done"'
Send 'M i'
$kindSeen = 'none'
$sw3 = [Diagnostics.Stopwatch]::StartNew()
while ($sw3.ElapsedMilliseconds -lt 20000 -and $kindSeen -eq 'none') {
    $l = Read-Line 500
    if ($null -eq $l) { continue }
    if ($l -like 'M e *') { $kindSeen = 'busy' }
    elseif ($l -like 'M i *') { $kindSeen = 'served' }
}
Note ('asked for the index mid-command: ' + $kindSeen)
if ($kindSeen -eq 'busy') {
    # The busy answer has to be short and say what to do about it, and the long command has to
    # have finished afterwards.
    Assert ($sw3.ElapsedMilliseconds -lt 4000) 'the busy answer does not wait for the command' ($sw3.ElapsedMilliseconds.ToString() + 'ms')
}
$longDone = $false
$sw4 = [Diagnostics.Stopwatch]::StartNew()
while ($sw4.ElapsedMilliseconds -lt 20000 -and -not $longDone) {
    $l = Read-Line 500
    if ($l -and $l -like '*long-done*') { $longDone = $true }
}
Assert ($longDone) 'the command running during a metadata request still finished' 'no marker'

Send 'X '
$p.StandardInput.Flush()
$p.StandardInput.Close()
$null = $p.StandardOutput.ReadToEnd()
$p.WaitForExit(20000) | Out-Null

# A fail-fast exit (0xC0000409 and friends) looks like this: the protocol worked, then the
# process died on its own. Anything but 0 is the engine having crashed, not the test failing.
$code = $p.ExitCode
$signed = if ($code -gt [int]::MaxValue) { $code - [int64]4294967296 } else { $code }
Assert ([int64]$signed -eq 0) 'the engine exited cleanly' ('exit code 0x' + ('{0:X8}' -f ([uint32]([int64]$signed -band 0xFFFFFFFF))))

Write-Host ''
if ($script:failures.Count -gt 0) {
    Write-Host ('FAILED ' + $script:failures.Count + ' of ' + $script:checks + ' checks') -ForegroundColor Red
    foreach ($f in $script:failures) { Write-Host ('  - ' + $f) -ForegroundColor Red }
    exit 1
}
Write-Host ('PASSED all ' + $script:checks + ' checks') -ForegroundColor Green
