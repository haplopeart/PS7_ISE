# Lists the windows the ISE process owns, with class, size and position of each. Used to measure
# whether the panes grew when the window was maximized - the screenshot shows it, the numbers say
# it precisely.
#
# The harness folder under the user temp directory is not a place to keep scripts: it is wiped
# between sessions, so every script written there has to be written again. This one lives in the
# repository instead, in the folder the repository already keeps its build scripts in.
param([string]$Class = 'NativeWindowsGuiWindow')
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public class Windows {
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr h, uint relationship);
    [DllImport("user32.dll")] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowTextW(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT area);

    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int l, t, r, b; }
    public class Win {
        public IntPtr Hwnd; public uint Pid; public string Class = ""; public string Text = "";
        public bool Visible; public bool Minimized; public int X, Y, W, H; public string Owner = "-";
    }
    const uint GW_OWNER = 4;

    public static List<Win> All() {
        var list = new List<Win>();
        EnumProc cb = (h, l) => {
            uint pid; GetWindowThreadProcessId(h, out pid);
            var cls = new StringBuilder(128); GetClassName(h, cls, 128);
            var txt = new StringBuilder(256); GetWindowTextW(h, txt, 256);
            RECT area; GetWindowRect(h, out area);
            var owner = GetWindow(h, GW_OWNER);
            list.Add(new Win {
                Hwnd = h, Pid = pid, Class = cls.ToString(), Text = txt.ToString(),
                Visible = IsWindowVisible(h), Minimized = IsIconic(h),
                X = area.l, Y = area.t, W = area.r - area.l, H = area.b - area.t,
                Owner = owner == IntPtr.Zero ? "-" : owner.ToString()
            });
            return true;
        };
        EnumWindows(cb, IntPtr.Zero);
        return list;
    }
}
'@
foreach ($w in [Windows]::All()) {
    if ($w.Class -ne $Class) { continue }
    "{0,-24} pid={1,-7} visible={2,-5} min={3,-5} owner={4,-9} {5}x{6} at {7},{8} '{9}'" -f `
        $w.Class, $w.Pid, $w.Visible, $w.Minimized, $w.Owner, $w.W, $w.H, $w.X, $w.Y, $w.Text
}