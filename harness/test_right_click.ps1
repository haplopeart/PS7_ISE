<#
.SYNOPSIS
    Drives right-click copy and paste in the Script pane and the Prompt pane.

.DESCRIPTION
    A right click on either pane copies what is highlighted and, when nothing is, pastes what the
    clipboard holds. Neither half can be proven by reading the panes back, because RichEdit content
    is not readable from another process, so this test works from the outside in:

      - The copy is proven through the clipboard, the one thing a right click writes that a test
        process can read. The selection is made with EM_SETSEL, a message the control answers for
        itself.
      - The paste is proven by what the window does with the text afterwards: pasted into the
        prompt, submitted, and run, the command leaves a file behind that only a run could leave.
      - Which of the two verbs the click chose is read from the in-process log, written when
        PS7ISE_DEBUG=1. That choice is the handler's whole job; the rest is the control's.

    The click is a WM_CONTEXTMENU sent to the control: the message a right click produces once the
    button comes up, and the one the handler answers. A real button cannot be pressed from here -
    Windows refuses to inject input into a window that does not own the keyboard.

.EXAMPLE
    .\harness\test_right_click.ps1
    .\harness\test_right_click.ps1 -Exe dist\PS7_ISE.exe -KeepOpen
#>
param(
    [string]$Exe = 'target\debug\PS7_ISE.exe',
    # Leaves the window open afterwards for a look by hand.
    [switch]$KeepOpen,
    # Milliseconds to let the window settle after each step.
    [int]$Settle = 1000
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root $Exe
if (-not (Test-Path $exe)) { throw "No executable at $exe - run cargo build or build.ps1 first." }

$target = Join-Path $env:TEMP 'ps7ise-mouse.ps1'
$log = Join-Path $env:TEMP 'ps7ise.log'
$inject = Join-Path $env:TEMP 'ps7ise-inject.txt'
$shots = Join-Path $env:TEMP 'ps7ise-shots'
# What the pasted command writes when it runs: the proof that survives leaving the window.
$marker = Join-Path $env:TEMP 'ps7ise-paste-ran.txt'

# Text the copy test looks for, and nothing that runs: this file is opened, never executed.
$body = @'
$one = 'first line of the script pane'
$two = 'second line of the script pane'
$three = 'third line of the script pane'
'@
Set-Content -Path $target -Encoding ASCII -Value $body

New-Item -ItemType Directory -Force -Path $shots | Out-Null
Remove-Item $log, $inject, $marker -ErrorAction SilentlyContinue
Get-Process PS7_ISE, PowerShellEngineHost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;

public static class Mouse {
    public delegate bool EnumProc(IntPtr h, IntPtr l);

    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr p, EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT rc);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    // SendMessageTimeout rather than SendMessage: this talks to a live GUI process, and a window
    // that is busy should cost one skipped check rather than the whole harness.
    [DllImport("user32.dll")] public static extern IntPtr SendMessageTimeoutW(
        IntPtr h, uint msg, IntPtr w, IntPtr l, uint flags, uint ms, out IntPtr result);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }

    const uint SMTO_ABORTIFHUNG = 0x0002;

    public static IntPtr Send(IntPtr h, uint msg, int w, int l) {
        IntPtr result;
        SendMessageTimeoutW(h, msg, new IntPtr(w), new IntPtr(l), SMTO_ABORTIFHUNG, 3000, out result);
        return result;
    }

    /// The ISE window belonging to `process`, or any of them for 0.
    ///
    /// The title bar alone is not enough. A previous run's window can still be on screen while its
    /// process is taken down, and its handles are good enough to enumerate and to ask the bounds of,
    /// but not to send to: a message to a window whose thread has gone faults the sender, which is
    /// a poor way for a test to die and then blame the program it was testing.
    public static IntPtr ForProcess(uint process) {
        var found = IntPtr.Zero;
        EnumWindows((h, l) => {
            if (!IsWindowVisible(h)) { return true; }
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (process != 0 && pid != process) { return true; }
            var text = new StringBuilder(256);
            GetWindowTextW(h, text, 256);
            if (text.ToString().Contains("PowerShell 7 ISE")) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    /// The two RichEdit panes, topmost first: the script pane sits above the console pane.
    public static List<IntPtr> Panes(IntPtr parent) {
        var list = new List<IntPtr>();
        EnumChildWindows(parent, (h, l) => {
            var name = new StringBuilder(256);
            GetClassNameW(h, name, 256);
            if (name.ToString().StartsWith("RICHEDIT")) { list.Add(h); }
            return true;
        }, IntPtr.Zero);
        list.Sort((a, b) => {
            RECT ra; GetWindowRect(a, out ra);
            RECT rb; GetWindowRect(b, out rb);
            return ra.Top.CompareTo(rb.Top);
        });
        return list;
    }

    public static string Bounds(IntPtr h) {
        RECT rc; GetWindowRect(h, out rc);
        return string.Format("{0}x{1} @ {2},{3}", rc.Right - rc.Left, rc.Bottom - rc.Top, rc.Left, rc.Top);
    }
}
'@

$EM_SETSEL = 0x00B1
# The message id is worth a second look: 0x00B0 is EM_GETSEL, the read, and only 0x00B1 is
# EM_SETSEL. A harness that selects with the first one "works" silently - the send succeeds, the
# control answers the question it was asked, and no text is highlighted - and the right click
# underneath it then chooses paste and the test fails somewhere other than where it broke.
$WM_CONTEXTMENU = 0x007B
# A pasted command that leaves a file behind, and two names for it: one for the Ctrl+V control run,
# one for the right click itself, so a failure says which of the two did not paste.
function PasteCommand([string]$name) {
    $path = Join-Path $env:TEMP "ps7ise-$name.txt"
    Remove-Item $path -ErrorAction SilentlyContinue
    return @{ Path = $path; Line = "Set-Content -LiteralPath '$path' -Value ran" }
}

$script:passed = 0
$script:failed = 0
$script:window = [IntPtr]::Zero

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

function Window {
    # The window of the process this harness started, never a leftover from the last one.
    if ($script:window -ne [IntPtr]::Zero) { return $script:window }
    $script:window = [Mouse]::ForProcess([uint32]$process.Id)
    return $script:window
}

function Send([string]$keys) {
    # The window reads this file and empties it; writing it is the only way in.
    Set-Content -Path $inject -Value $keys -NoNewline -Encoding ASCII
    Start-Sleep -Milliseconds $Settle
}

function Shot([string]$name) {
    $window = Window
    if ($window -eq [IntPtr]::Zero) { Write-Warning 'no window to photograph'; return }
    $rect = New-Object 'Mouse+RECT'
    [void][Mouse]::GetWindowRect($window, [ref]$rect)
    $wide = $rect.Right - $rect.Left
    $high = $rect.Bottom - $rect.Top
    if ($wide -le 0 -or $high -le 0) { Write-Warning "window is ${wide}x${high}"; return }
    $bitmap = New-Object System.Drawing.Bitmap($wide, $high)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $hdc = $graphics.GetHdc()
    # PW_RENDERFULLCONTENT = 2: the panes are RichEdit controls and their pixels are not in the
    # window's own surface, so a plain BitBlt would catch whatever is behind it.
    [void][Mouse]::PrintWindow($window, $hdc, 2)
    $graphics.ReleaseHdc($hdc)
    $path = Join-Path $shots "$name.png"
    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "  shot $path" -ForegroundColor DarkGray
}

function Verb([string]$pane) {
    # The verb of the newest "right click <pane>: copy|paste" the window wrote.
    $lines = @(Get-Content $log -ErrorAction SilentlyContinue | Where-Object { $_ -like "right click $pane*" })
    if ($lines.Count -eq 0) { return '' }
    return ($lines[-1] -replace '^.*:\s*', '').Trim()
}

$env:PS7ISE_DEBUG = '1'
Write-Host "== starting $exe with $target" -ForegroundColor Cyan
$process = Start-Process $exe -ArgumentList "`"$target`"" -PassThru

try {
    $ready = $false
    foreach ($attempt in 1..40) {
        Start-Sleep -Milliseconds 500
        if ((Get-Content $log -ErrorAction SilentlyContinue) -match 'engine started') { $ready = $true; break }
    }
    if (-not $ready) { throw "the engine never reported ready; see $log" }
    Start-Sleep -Milliseconds 1500

    $window = Window
    if ($window -eq [IntPtr]::Zero) { throw "no window for pid $($process.Id)" }
    $panes = @([Mouse]::Panes($window))
    if ($panes.Count -lt 2) { throw "expected two RichEdit panes, found $($panes.Count)" }
    $editor, $console = $panes[0], $panes[1]
    Write-Host "  script pane $([Mouse]::Bounds($editor))" -ForegroundColor DarkGray
    Write-Host "  prompt pane $([Mouse]::Bounds($console))" -ForegroundColor DarkGray

    # ------------------------------------------------------------ Script pane: copy
    Write-Host "`n-- Right click on selected text in the Script pane copies it" -ForegroundColor Yellow
    Set-Clipboard -Value 'nothing was copied'
    # EM_SETSEL from 0 to the end of the text. The "-1 selects everything" form is an edit control
    # convention this control does not honour: measured, the pane went on reporting no selection
    # and the click pasted instead of copying.
    [void][Mouse]::Send($editor, $EM_SETSEL, 0, -1)
    [void][Mouse]::Send($editor, $WM_CONTEXTMENU, 0, 0)
    Start-Sleep -Milliseconds 800
    $copied = (Get-Clipboard) -join "`n"
    Check 'the pane chose copy' ((Verb 'editor') -eq 'copy') (Verb 'editor')
    Check 'the clipboard holds the script' `
        ($copied.Replace("`r`n", "`n").Trim() -eq $body) ($copied -replace "`r`n", ' / ')
    Shot '10-editor-copy'

    # ------------------------------------------------------------ Script pane: paste
    Write-Host "`n-- Right click with nothing selected in the Script pane pastes" -ForegroundColor Yellow
    Set-Clipboard -Value '$pastedByRightClick = 1'
    # Caret at 0 with an empty selection: both ends are given, so no highlight can survive into the
    # checks below and be taken for a copy.
    [void][Mouse]::Send($editor, $EM_SETSEL, 0, 0)
    [void][Mouse]::Send($editor, $WM_CONTEXTMENU, 0, 0)
    Start-Sleep -Milliseconds 800
    Check 'the pane chose paste' ((Verb 'editor') -eq 'paste') (Verb 'editor')
    Shot '11-editor-paste'

    # ------------------------------------------------------------ Prompt pane: paste
    # Ctrl+V is the control run. It reaches the same `paste()` the right click reaches, so when it
    # pastes and the click does not, the fault is in the click, and when neither does it is in the
    # paste - and those are very different things to go looking for.
    Write-Host "`n-- Control: Ctrl+V at the prompt, the same paste the click will ask for" -ForegroundColor Yellow
    $control = PasteCommand 'paste-ctrlv'
    Set-Clipboard -Value $control.Line
    Send '{CTRL_V}'
    Start-Sleep -Milliseconds 800
    Shot '12a-prompt-ctrlv'
    Send '{ENTER}'
    Start-Sleep -Milliseconds 2500
    Check 'Ctrl+V pasted a line that ran' (Test-Path $control.Path) $control.Path

    Write-Host "`n-- Right click at the prompt pastes into the line being typed" -ForegroundColor Yellow
    $click = PasteCommand 'paste-rmb'
    Set-Clipboard -Value $click.Line
    [void][Mouse]::Send($console, $WM_CONTEXTMENU, 0, 0)
    Start-Sleep -Milliseconds 800
    Check 'the pane chose paste' ((Verb 'console') -eq 'paste') (Verb 'console')
    Shot '12-prompt-pasted'

    Write-Host "-- ... and what was pasted runs, which is how we know it reached the prompt" -ForegroundColor Yellow
    Send '{ENTER}'
    Start-Sleep -Milliseconds 2500
    Check 'the pasted command ran' (Test-Path $click.Path) $click.Path
    Shot '13-prompt-ran'

    # ------------------------------------------------------------ Prompt pane: nothing to copy
    Write-Host "`n-- Right click with nothing selected at the prompt: paste, clipboard untouched" -ForegroundColor Yellow
    Set-Clipboard -Value 'still here'
    [void][Mouse]::Send($console, $WM_CONTEXTMENU, 0, 0)
    Start-Sleep -Milliseconds 800
    Check 'the pane chose paste' ((Verb 'console') -eq 'paste') (Verb 'console')
    Check 'the clipboard still holds what it held' ((Get-Clipboard) -eq 'still here') (Get-Clipboard)
    # Take the pasted text back out of the prompt: Ctrl+C with no selection clears the line, which
    # is what the user would press.
    Send '{CTRL_C}'
}
finally {
    Write-Host "`n== $script:passed passed, $script:failed failed" -ForegroundColor Cyan
    if ($KeepOpen) {
        Write-Host 'left open: close the window when you have looked at it' -ForegroundColor Yellow
        Wait-Process -Id $process.Id -ErrorAction SilentlyContinue
    }
    else {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    }
    Get-Process PowerShellEngineHost -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Write-Host "screenshots in $shots" -ForegroundColor DarkGray
}
