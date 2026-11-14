using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using Microsoft.Win32.SafeHandles;

/// <summary>
/// Gives native commands somewhere to talk to.
///
/// The protocol rides on the engine's standard output, and a program PowerShell starts inherits
/// exactly that handle. So <c>DISM</c>, <c>MpCmdRun.exe</c>, <c>ipconfig</c> - anything that is
/// not a cmdlet - wrote its report straight into the protocol stream, where the reader did not
/// recognise the lines and dropped them. The command still ran; the user saw nothing, which for
/// a maintenance script is indistinguishable from a failure.
///
/// Before PowerShell starts, the standard handles are pointed at pipes of our own. Stdout and
/// stderr drain onto the protocol as ordinary output and error lines, so native text appears the
/// way it does in a real console, in red where it belongs. Stdin becomes a pipe whose write end
/// is already closed: a command that waits for the keyboard (<c>pause</c>, a tool that prompts)
/// would otherwise read the engine's own command records, swallowing the record meant to stop it
/// or hanging until one turns up. Closed at birth, it reads end-of-input and carries on, which is
/// what a batch job wants.
///
/// Only what children inherit changes. Program had already given .NET's own Console writes to
/// standard error, and a stream keeps the handle it was opened with.
/// </summary>
public static class NativeConsole
{
    private const int STD_INPUT_HANDLE = -10;
    private const int STD_OUTPUT_HANDLE = -11;
    private const int STD_ERROR_HANDLE = -12;

    [StructLayout(LayoutKind.Sequential)]
    private sealed class Inheritable
    {
        public int Length;
        public IntPtr SecurityDescriptor;
        public bool InheritHandle;

        public Inheritable()
        {
            Length = Marshal.SizeOf(typeof(Inheritable));
            SecurityDescriptor = IntPtr.Zero;
            InheritHandle = true;
        }
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CreatePipe(out IntPtr readHandle, out IntPtr writeHandle,
                                          Inheritable securityAttributes, int size);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetStdHandle(int which, IntPtr handle);

    /// <summary>
    /// A pipe whose write end children can inherit. The read end is handed to a pump; the write
    /// end deliberately stays open here, so the pump only sees end-of-input when the process does.
    /// </summary>
    private static IntPtr NewPipe(out IntPtr readHandle)
    {
        if (!CreatePipe(out readHandle, out IntPtr writeHandle, new Inheritable(), 64 * 1024))
        {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }
        return writeHandle;
    }

    /// <summary>
    /// Points the three standard handles at private pipes and starts pumping two of them.
    /// Call before any PowerShell exists: handles are inherited when a process is created, so a
    /// command already running would keep the protocol pipe it was born with.
    /// </summary>
    /// <param name="onOutputLine">One line a native program wrote to stdout.</param>
    /// <param name="onErrorLine">One line a native program wrote to stderr.</param>
    public static void Start(Action<string> onOutputLine, Action<string> onErrorLine)
    {
        IntPtr stdout = NewPipe(out IntPtr outRead);
        IntPtr stderr = NewPipe(out IntPtr errRead);

        // Stdin is the opposite of the other two: the child gets the *read* end and the write end
        // is closed for good, so a command that waits for the keyboard (pause, a tool that
        // prompts) reads end-of-input at once and carries on. Handing it a write end instead
        // would make that read wait forever on the child's own handle, and handing it the
        // protocol's real stdin would let it swallow the record meant to stop it.
        IntPtr stdinWriter = NewPipe(out IntPtr stdin);
        CloseHandle(stdinWriter);

        if (!SetStdHandle(STD_OUTPUT_HANDLE, stdout) ||
            !SetStdHandle(STD_ERROR_HANDLE, stderr) ||
            !SetStdHandle(STD_INPUT_HANDLE, stdin))
        {
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        }

        Pump(outRead, onOutputLine, "PS7-ISE-NativeOut");
        Pump(errRead, onErrorLine, "PS7-ISE-NativeErr");
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr handle);

    /// <summary>
    /// Drains one pipe on a thread of its own. A pump that throws must not take the shell with it:
    /// losing native output is bad, losing the prompt is worse, so anything unexpected is written
    /// to standard error, which the front end keeps as a log.
    /// </summary>
    private static void Pump(IntPtr readHandle, Action<string> onLine, string name)
    {
        NativeOutputPump pump = new NativeOutputPump(readHandle, onLine);
        Thread thread = new Thread(() =>
        {
            try
            {
                pump.Run();
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine(name + ": " + ex.Message);
            }
        })
        {
            IsBackground = true,
            Name = name,
        };
        thread.Start();
    }
}
