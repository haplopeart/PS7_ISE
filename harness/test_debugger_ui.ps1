<#
.SYNOPSIS
    Drives the debugger front end end to end and keeps a screenshot of every step.

.DESCRIPTION
    The console pane cannot be driven by posted keystrokes - Windows refuses input to a
    window without the keyboard focus - so this runs a debug build with PS7ISE_DEBUG=1 and
    writes its keystrokes to %TEMP%\ps7ise-inject.txt, which the window replays on its next
    tick. `{LINE:n}` puts the caret on a script line, `{F9}` and `{F5}` are the real keys
    through the same routing the keyboard takes, and `{MENU:n}` presses a menu item by
    command id.

    RichEdit content cannot be read back from another process, so the evidence is the
    screenshots plus the in-process log, where every debugger request and stop is written.

    The script being debugged is written to %TEMP% and passed on the command line, because a
    PowerShell line breakpoint belongs to a file on disk: an unsaved tab cannot carry one.

.EXAMPLE
    .\harness\test_debugger_ui.ps1
    .\harness\test_debugger_ui.ps1 -Exe dist\PS7_ISE.exe -KeepOpen
#>
param(
    [string]$Exe = 'target\debug\PS7_ISE.exe',
    # Leaves the window open afterwards for a look by hand.
    [switch]$KeepOpen,
    # Prints what the window reader sees before the first key: handle, every child with its
    # visibility, and the text of each. Written because a run that reports every check failed has
    # to say whether the window was gone or only unreadable, and those are different bugs.
    [switch]$Diag,
    # Milliseconds to let the shell settle after each key. A step is a round trip into the
    # runspace and back out again, and a fraction of this has never been enough.
    [int]$Settle = 1200
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root $Exe
if (-not (Test-Path $exe)) { throw "No executable at $exe - run cargo build or build.ps1 first." }

$target = Join-Path $env:TEMP 'ps7ise-dbg-e2e.ps1'
$log = Join-Path $env:TEMP 'ps7ise.log'
$inject = Join-Path $env:TEMP 'ps7ise-inject.txt'
$shots = Join-Path $env:TEMP 'ps7ise-shots'

# Line 6 is the call: a breakpoint there stops with two frames once it is stepped into, and
# with a variable list worth reading. The last line leaves a file behind, which is how a test knows
# the script ran to its end — the console pane is RichEdit, so its text is not readable from here.
# Written at the prompt *after* a run, from the variables the script made. A dot-sourced script
# leaves its variables in the session, and this is the check that it does, in the direction the
# user cares about: not that the script saw its own variables, but that the prompt can still see
# them afterwards.
$leaked = Join-Path $env:TEMP 'ps7ise-dbg-e2e-leaked.txt'
$marker = Join-Path $env:TEMP 'ps7ise-dbg-e2e-ran.txt'
Set-Content -Path $target -Encoding ASCII -Value @"
function Get-Total {
    param(`$a, `$b)
    `$sum = `$a + `$b
    return `$sum
}
`$result = Get-Total 3 4
`$where = `$PSScriptRoot
Write-Host "TOTAL=`$result"
Set-Content -LiteralPath '$marker' -Value "`$result|`$where"
"@

New-Item -ItemType Directory -Force -Path $shots | Out-Null
# The marker files from an earlier run go first. A marker left behind by the last run is the kind
# of failure a test cannot see: it says the script ran, and nothing about this run is being read.
Remove-Item $log, $inject, $marker, $leaked -ErrorAction SilentlyContinue
Get-Process PS7_ISE, PowerShellEngineHost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;

public static class Win {
    public delegate bool EnumProc(IntPtr h, IntPtr l);

    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr p, EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT rc);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

    public static List<IntPtr> Top() {
        var list = new List<IntPtr>();
        EnumWindows((h, l) => { if (IsWindowVisible(h)) list.Add(h); return true; }, IntPtr.Zero);
        return list;
    }

    /// The ISE window, found by its title bar.
    public static IntPtr Main() {
        foreach (var top in Top()) {
            var text = new StringBuilder(256);
            GetWindowTextW(top, text, 256);
            if (text.ToString().Contains("PowerShell 7 ISE")) { return top; }
        }
        return IntPtr.Zero;
    }

    /// Every child as "class WxH @ x,y shown|hidden", which is how a test tells the Debugger
    /// column's lists from the Commands column's.
    public static List<string> Children(IntPtr parent) {
        var rows = new List<string>();
        EnumChildWindows(parent, (h, l) => {
            var name = new StringBuilder(256);
            GetClassNameW(h, name, 256);
            RECT rc; GetWindowRect(h, out rc);
            rows.Add(string.Format("{0} {1}x{2} @ {3},{4} {5}", name, rc.Right - rc.Left,
                rc.Bottom - rc.Top, rc.Left, rc.Top, IsWindowVisible(h) ? "shown" : "hidden"));
            return true;
        }, IntPtr.Zero);
        return rows;
    }

    public static string Bounds(IntPtr h) {
        RECT rc; GetWindowRect(h, out rc);
        return string.Format("{3}x{4} @ {1},{2}", h, rc.Left, rc.Top, rc.Right - rc.Left, rc.Bottom - rc.Top);
    }

    /// Child handles alone: the reader wants the windows, not a line of text about them.
    public static List<IntPtr> ChildHandles(IntPtr parent) {
        var list = new List<IntPtr>();
        EnumChildWindows(parent, (h, l) => { list.Add(h); return true; }, IntPtr.Zero);
        return list;
    }

    public static string Class(IntPtr h) {
        var name = new StringBuilder(256);
        GetClassNameW(h, name, 256);
        return name.ToString();
    }
}
'@

function Send([string]$keys) {
    # The window reads this file and empties it; writing it is the only way in.
    Set-Content -Path $inject -Value $keys -NoNewline -Encoding ASCII
    Start-Sleep -Milliseconds $Settle
}

# ---------------------------------------------------------------------- assertions
#
# Nothing in this window can be read from another process: the two panes are RichEdit, whose text is
# a lie to anyone outside, and measured here the strip's labels, list boxes and edits answered
# `GetWindowText` with nothing as well - a whole window of controls reported empty. So the window is
# asked to say what it is holding: the `{DUMP}` token makes it write the strings `repaint` handed its
# controls into the log, which is the channel this program already trusts for the same reason.
# Screenshots still go with every step, because the log says what the program believes it showed and
# a picture says what came out.

$script:passed = 0
$script:failed = 0

function Check([string]$what, [bool]$ok, [string]$detail = '') {
    if ($ok) {
        $script:passed++
        Write-Host "  ok   $what" -ForegroundColor Green
        return
    }
    $script:failed++
    $suffix = if ($detail) { "  [$detail]" } else { '' }
    Write-Host "  FAIL $what$suffix" -ForegroundColor Red
}

function LogLines([string]$like) {
    @(Get-Content $log -ErrorAction SilentlyContinue | Where-Object { $_ -like $like })
}

function WaitLog([string]$like, [int]$Milliseconds = 5000) {
    # Waits for a line to show up and hands back every one of them. A check on the log has to wait
    # for the fact rather than for a sleep: a fixed pause is a guess about when the window got round
    # to writing it down, and a test that guesses wrong reports a fault the program never had.
    $deadline = (Get-Date).AddMilliseconds($Milliseconds)
    while ((Get-Date) -lt $deadline) {
        $rows = LogLines $like
        if ($rows.Count) { return $rows }
        Start-Sleep -Milliseconds 150
    }
    return @(LogLines $like)
}

function WaitFile([string]$Path, [int]$Milliseconds = 8000) {
    # Same argument as WaitLog, for the facts a script leaves on disk: the console pane's text is
    # RichEdit and unreadable from here, so what a command proved is a file it wrote.
    $deadline = (Get-Date).AddMilliseconds($Milliseconds)
    while ((Get-Date) -lt $deadline) {
        if (Test-Path $Path) { return (Get-Content $Path -ErrorAction SilentlyContinue) -join '' }
        Start-Sleep -Milliseconds 150
    }
    return ''
}

function Stops {
    LogLines 'debug: stopped*'
}

function Snapshot {
    # Asks the window what the Debugger column holds and hands the answer back both ways: as the rows
    # of each part, and as one string a check can search.
    #
    # The wait is for the `dump: begin` line rather than for a fixed sleep, because a dump taken
    # before the previous step's answer arrived describes the stop before it - which is a way for a
    # test to pass by being early.
    $before = (LogLines 'dump: begin').Count
    Set-Content -Path $inject -Value '{DUMP}' -NoNewline -Encoding ASCII
    foreach ($attempt in 1..20) {
        Start-Sleep -Milliseconds 250
        if ((LogLines 'dump: begin').Count -gt $before) { break }
    }

    $all = @(Get-Content $log -ErrorAction SilentlyContinue)
    $start = $all.Length
    for ($i = $all.Length - 1; $i -ge 0; $i--) {
        if ($all[$i] -like 'dump: begin*') { $start = $i; break }
    }
    $rows = @($all[$start..($all.Length - 1)] | Where-Object { $_ -like 'dump: *' })
    $pick = {
        param([string]$prefix)
        @($rows | Where-Object { $_ -like "$prefix*" } | ForEach-Object { $_.Substring($prefix.Length) })
    }

    [pscustomobject]@{
        Text      = $rows -join "`n"
        Visible   = [bool](@($rows | Where-Object { $_ -eq 'dump: visible true' }).Count)
        Status    = (& $pick 'dump: status ') -join '; '
        Heading   = (& $pick 'dump: heading ') -join '; '
        Stack     = & $pick 'dump: stack '
        Variables = & $pick 'dump: variable '
        Answers   = & $pick 'dump: answer '
    }
}


function Shot([string]$name) {
    $window = [Win]::Main()
    if ($window -eq [IntPtr]::Zero) { Write-Warning 'no window to photograph'; return }
    $rect = New-Object 'Win+RECT'
    [void][Win]::GetWindowRect($window, [ref]$rect)
    $wide = $rect.Right - $rect.Left
    $high = $rect.Bottom - $rect.Top
    if ($wide -le 0 -or $high -le 0) { Write-Warning "window is ${wide}x${high}"; return }
    $bitmap = New-Object System.Drawing.Bitmap($wide, $high)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $hdc = $graphics.GetHdc()
    # PW_RENDERFULLCONTENT: the panes are RichEdit controls and their pixels are not in the
    # window's own surface, so a plain BitBlt of the screen would catch whatever is behind it.
    [void][Win]::PrintWindow($window, $hdc, 2)
    $graphics.ReleaseHdc($hdc)
    $path = Join-Path $shots "$name.png"
    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "  shot $path" -ForegroundColor DarkGray
}

$env:PS7ISE_DEBUG = '1'
Write-Host "== starting $exe with $target" -ForegroundColor Cyan
$process = Start-Process $exe -ArgumentList "`"$target`"" -PassThru

try {
    # The engine takes a few seconds to open its runspace, and the breakpoint list is only
    # asked for once there is a shell to ask.
    $ready = $false
    foreach ($attempt in 1..40) {
        Start-Sleep -Milliseconds 500
        if ((Get-Content $log -ErrorAction SilentlyContinue) -match 'engine started') { $ready = $true; break }
    }
    if (-not $ready) { throw "the engine never reported ready; see $log" }
    Start-Sleep -Milliseconds 1000

    if ($Diag) {
        # What the window is before any key is pressed. The control list is the cheap way to see a
        # layout gone wrong - a hidden control still reports its geometry, so a column that was
        # never placed sits at its parent's origin - and the snapshot is the same window's own
        # account of what it believes it is showing.
        $window = [Win]::Main()
        Write-Host "  DIAG window $window"
        if ($window -ne [IntPtr]::Zero) {
            [Win]::Children($window) | ForEach-Object { "  DIAG $_" }
        }
        (Snapshot).Text -split "`n" | ForEach-Object { "  DIAG $_" }
    }

    Write-Host '-- Step Over at the prompt: nothing is stopped, so nothing happens' -ForegroundColor Yellow
    $stops = @(Stops).Count
    Send '{MENU:53}'
    # The bar has to say why nothing moved: a key that appears to do nothing is indistinguishable
    # from a key that was not received, and the user has no way to tell which happened.
    $idle = Snapshot
    Check 'the column says it is not debugging' ($idle.Status -eq 'Not debugging') $idle.Status
    Check 'and keeps the column to itself until there is something to show' (-not $idle.Visible) 'shown before any stop'
    Check 'and the bar says there is nothing to step through' `
        ((@(LogLines 'status: The shell is not stopped*').Count -ge 1)) 'the bar said nothing'
    Check 'and no stop was invented' (@(Stops).Count -eq $stops) ((Stops) -join '; ')
    Shot '01-step-at-prompt'

    Write-Host '-- F9 on line 6: a breakpoint, and a dot in the margin' -ForegroundColor Yellow
    Send '{LINE:6}'
    Send '{F9}'
    Check 'the breakpoint was asked for on line 6' `
        ((@(WaitLog 'debug: b +6*').Count -ge 1)) 'no "b +6" in the log'
    Shot '02-breakpoint-set'

    Write-Host '-- F5: the script runs and comes to rest on the breakpoint' -ForegroundColor Yellow
    Send '{F5}'
    Start-Sleep -Milliseconds 2500
    $stop = Snapshot
    Check 'the shell stopped on the breakpoint' `
        ((@(LogLines 'debug: stopped (breakpoint)*').Count -ge 1)) 'no stop in the log'
    Check 'the column says where execution came to rest' `
        ($stop.Status -eq 'Stopped at line 6 of ps7ise-dbg-e2e.ps1') $stop.Status
    Check 'the column is shown now' ($stop.Visible) 'still hidden after a stop'
    Check 'the call stack holds the script' `
        ((@($stop.Stack -match 'ps7ise-dbg-e2e.ps1').Count -ge 1)) ($stop.Stack -join ' ; ')
    Check 'the bar names the stop and the keys that let it go' `
        ((@(LogLines 'status: Debugger stopped at line 6*').Count -ge 1)) 'the bar said nothing about it'
    Shot '03-stopped'

    Write-Host '-- F11: into the call, two frames deep' -ForegroundColor Yellow
    Send '{F11}'
    $stop = Snapshot
    Check 'stepping in stopped again' (@(Stops).Count -ge 2) 'only one stop in the log'
    # Three frames, not two: Run Script dot-sources the file, so the call that started it is a
    # frame of its own at the bottom of the stack. This is PowerShell's own stack reported as it
    # found it - Get-Total, the script, and the dot-source at the prompt - and the harness names the
    # number here because a stack that silently grows by one is a stack nobody is reading.
    Check 'the column counts the frames' ($stop.Heading -eq 'Frame 1 of 3') $stop.Heading
    Check 'the stack names the function' `
        ((@($stop.Stack -match 'Get-Total').Count -ge 1)) ($stop.Stack -join ' ; ')
    Check 'and says that this stop came from stepping' `
        ($stop.Status -like 'Stopped at line * (stepping)') $stop.Status
    Shot '04-step-into'

    Write-Host '-- Step Over twice: $sum appears in the frame' -ForegroundColor Yellow
    Send '{MENU:53}'
    Send '{MENU:53}'
    $stop = Snapshot
    # The frame's own three names, and nothing else. What a reader of this frame wants is what the
    # function made: not the session's few hundred, which is what a plain read of the scope answers
    # with, and not the handful the read itself needed to make. Both of those are what earlier
    # versions of this listed, and both were found here rather than by a user.
    $names = @($stop.Variables | Where-Object { $_ -like '$*' })
    Check 'the frame lists its two parameters' `
        ((@($names | Where-Object { $_ -match '^\$a \[Int32\] 3$' }).Count -eq 1) -and
         (@($names | Where-Object { $_ -match '^\$b \[Int32\] 4$' }).Count -eq 1)) ($names -join ' ; ')
    Check 'the frame lists what it worked out' `
        (@($names | Where-Object { $_ -match '^\$sum \[Int32\] 7$' }).Count -eq 1) ($names -join ' ; ')
    Check 'and nothing else' ($names.Count -eq 3) ($names -join ' ; ')
    Shot '05-step-over'

    Write-Host '-- Evaluate: a value and a name that is not there, into the Immediate box' -ForegroundColor Yellow
    Send '{EVAL:$a + $b}'
    Send '{EVAL:$nothing-here}'
    $stop = Snapshot
    Check 'the question is in the transcript' `
        ((@($stop.Answers | Where-Object { $_ -match '\$a \+ \$b' }).Count -ge 1)) ($stop.Answers -join ' ; ')
    Check 'the answer is the sum, with the type it came back as' `
        ((@($stop.Answers | Where-Object { $_ -match 'Int32\s+7$' }).Count -ge 1)) ($stop.Answers -join ' ; ')
    Check 'a name that is not there is answered in PowerShell words' `
        ((@($stop.Answers | Where-Object { $_ -match 'nothing-here' }).Count -ge 1)) ($stop.Answers -join ' ; ')
    # The wrapper the expression is run inside is nobody's business. Asked for `$nothing-here`, an
    # early build reported a parse error in the script that wrapped it - `At line:10 ... foreach
    # ($item in (& { ...`, which reads as a broken program rather than as a typo in the question.
    Check 'and no answer is about the wrapper' `
        ((@($stop.Answers -match 'foreach|& \{').Count -eq 0)) ($stop.Answers -join ' ; ')
    # Every row on a line of its own: the box no longer wraps its own text, so a transcript joined
    # with the wrong characters is one long line, question and answer run together.
    Check 'every row is a line of its own' `
        ((@($stop.Answers | Where-Object { $_ }).Count -ge 4)) ($stop.Answers -join ' ; ')
    Shot '05b-evaluate'

    Write-Host '-- Step Out: back in the script, one frame shallower' -ForegroundColor Yellow
    Send '{MENU:54}'
    $stop = Snapshot
    Check 'the stack gave up the function' ((@($stop.Stack -match 'Get-Total').Count -eq 0)) ($stop.Stack -join ' ; ')
    Check 'and the frame being read is the script' ($stop.Heading -eq 'Frame 1 of 2') $stop.Heading
    Shot '06-step-out'

    Write-Host '-- Continue: the stop ends and the script finishes' -ForegroundColor Yellow
    Send '{MENU:51}'
    Start-Sleep -Milliseconds 3000
    $stop = Snapshot
    Check 'the column says it is not debugging again' ($stop.Status -eq 'Not debugging') $stop.Status
    Check 'and the frame lists are empty rather than stale' (@($stop.Stack).Count -eq 0) ($stop.Stack -join ' ; ')
    Shot '07-resumed'


    Write-Host '-- F9 on the same line again: the breakpoint comes off' -ForegroundColor Yellow
    Send '{LINE:6}'
    Send '{F9}'
    Check 'the breakpoint was removed' ((@(WaitLog 'debug: b -*').Count -ge 1)) 'no "b -" in the log'
    Shot '08-breakpoint-off'

    # Run Script with nothing to stop on. Run dot-sources the file instead of handing its text to the
    # prompt - a breakpoint belongs to a path, so that is what makes debugging possible at all - and
    # dot-sourcing changes how a script runs even with no breakpoint in sight. Three things follow,
    # and all three are checked, because a change that makes debugging work must not quietly change
    # the ordinary case: the script reaches its own last line; it runs *as a file*, which is the only
    # way $PSScriptRoot is the script's folder rather than empty, as it is when the same text is
    # handed to the prompt; and the names it made belong to the session afterwards, which is what the
    # README tells people to expect from a saved tab.
    Write-Host '-- Run Script with no breakpoint: straight through, nothing stopped' -ForegroundColor Yellow
    $before = @(Stops).Count
    Remove-Item $marker -ErrorAction SilentlyContinue
    Send '{F5}'
    $ran = WaitFile $marker 8000
    $after = @(Stops).Count
    Check 'the script ran without stopping' ($after -eq $before) "$before stops before, $after after"
    Check 'the script ran to its own last line' ($ran -like '7|*') "the file says [$ran]"
    Check 'and it ran as a file, so $PSScriptRoot was its own folder' `
        ((($ran -split '\|')[1]) -eq $env:TEMP) "expected [$env:TEMP], the file says [$ran]"
    Shot '09-ran-through'

    Write-Host '-- The session still holds what the script made' -ForegroundColor Yellow
    Remove-Item $leaked -ErrorAction SilentlyContinue
    # Typed at the prompt once the run is long finished and the pane idle: the only way these two
    # names have values in it is that the script was dot-sourced into the session.
    $ask = "Set-Content -LiteralPath '$leaked' -Value `"`$result|`$where`" {ENTER}"
    Send $ask
    $left = WaitFile $leaked 8000
    Check 'the prompt can still read the names the script made' ($left -like '7|*') "the file says [$left]"
    Check 'and they are the values this run worked out' ($left -eq $ran) `
        "the run wrote [$ran], the prompt wrote [$left]"
    Shot '10-variables-survived'

    Write-Host "`n== $passed passed, $failed failed" -ForegroundColor $(if ($failed) { 'Red' } else { 'Green' })

    Write-Host "`n== the window, control by control" -ForegroundColor Cyan
    $window = [Win]::Main()
    if ($window -ne [IntPtr]::Zero) {
        Write-Host "  window $([Win]::Bounds($window))"
        [Win]::Children($window) | Where-Object { $_ -match 'LISTBOX|EDIT|STATIC|BUTTON' } |
            ForEach-Object { "  $_" }
    }

    Write-Host "`n== debugger traffic from $log" -ForegroundColor Cyan
    Get-Content $log | Select-String -Pattern 'debug:' | ForEach-Object { "  $($_.Line)" }

    Write-Host "`n== what the script printed when it was let go" -ForegroundColor Cyan
    Get-Content $log | Select-String -Pattern 'TOTAL=' | ForEach-Object { "  $($_.Line)" }
} finally {
    if ($KeepOpen) {
        Write-Host "`nleft running (pid $($process.Id)); screenshots in $shots" -ForegroundColor Green
    } else {
        Get-Process PS7_ISE, PowerShellEngineHost -ErrorAction SilentlyContinue |
            Stop-Process -Force -ErrorAction SilentlyContinue
    }
}
