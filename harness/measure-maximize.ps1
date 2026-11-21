# Measures the ISE panes before and after maximizing the window, so the fix can be checked with
# numbers rather than a screenshot. Enumerates the frame's child controls, prints the rectangle
# of every control that is at least 100 px in either direction, then sends WM_SYSCOMMAND
# SC_MAXIMIZE, waits, prints again, sends SC_RESTORE and prints a third time.
param([string]$Title = 'Windows PowerShell 7 ISE')
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public class Panes {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr h, EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr h, uint msg, IntPtr w, IntPtr l);
    [DllImport("user32.dll")] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT area);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int l, t, r, b; }
    public class Win {
        public IntPtr Hwnd; public uint Pid; public string Class = ""; public string Text = "";
        public bool Visible; public int X, Y, W, H;
    }
    const uint SC_MAXIMIZE = 0xF030;
    const uint SC_RESTORE = 0xF120;
    const uint WM_SYSCOMMAND = 0x0112;

    static Win Make(IntPtr h) {
        RECT area; GetWindowRect(h, out area);
        var cls = new StringBuilder(128); GetClassName(h, cls, 128);
        var txt = new StringBuilder(256); GetWindowTextW(h, txt, 256);
        return new Win {
            Hwnd = h, Class = cls.ToString(), Text = txt.ToString(),
            Visible = IsWindowVisible(h), X = area.l, Y = area.t,
            W = area.r - area.l, H = area.b - area.t
        };
    }

    /// The frame itself: the top-level window whose title starts with the given string.
    public static IntPtr Frame(string title) {
        var found = IntPtr.Zero;
        EnumProc cb = (h, l) => {
            var cls = new StringBuilder(128); GetClassName(h, cls, 128);
            var txt = new StringBuilder(256); GetWindowTextW(h, txt, 256);
            if (txt.ToString().StartsWith(title) && (cls.ToString() == "NativeWindowsGuiWindow" || cls.ToString() == "ApplicationFrameWindow")) found = h;
            return true;
        };
        EnumWindows(cb, IntPtr.Zero);
        return found;
    }

    /// Every child control, with its rectangle, that is big enough to be a pane.
    public static List<Win> Children(IntPtr parent) {
        var list = new List<Win>();
        EnumProc cb = (h, l) => {
            var w = Make(h);
            if (w.W > 100 || w.H > 100) list.Add(w);
            return true;
        };
        EnumChildWindows(parent, cb, IntPtr.Zero);
        list.Sort((a, b) => (a.Y * 10000 + a.X).CompareTo(b.Y * 10000 + b.X));
        return list;
    }

    public static void Maximize(IntPtr h) { SendMessageW(h, WM_SYSCOMMAND, (IntPtr)SC_MAXIMIZE, IntPtr.Zero); }
    public static void Restore(IntPtr h) { SendMessageW(h, WM_SYSCOMMAND, (IntPtr)SC_RESTORE, IntPtr.Zero); }
}
'@
function Show([string]$label) {
    "=== $label"
    foreach ($w in [Panes]::Children([Panes]::Frame($Title))) {
        "  {0,-16} {1,-10} {2}x{3} at {4},{5}" -f $w.Class, $w.Text, $w.W, $w.H, $w.X, $w.Y
    }
}

$frame = [Panes]::Frame($Title)
if ($frame -eq [IntPtr]::Zero) { Write-Error "no frame titled '$Title'"; exit 1 }
Show "before"
[Panes]::Maximize($frame)
Start-Sleep -Milliseconds 600
Show "maximized"
[Panes]::Restore($frame)
Start-Sleep -Milliseconds 600
Show "restored"