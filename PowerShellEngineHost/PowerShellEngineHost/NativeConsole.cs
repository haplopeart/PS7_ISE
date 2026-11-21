using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using Microsoft.Win32.SafeHandles;

/// <summary>
/// Gives native commands somewhere to talk to.
///
/// Protocol rides on engine stdout, so child process stdout/stderr must be captured and
/// re-emitted as protocol records. Stdin is redirected to an empty/closed pipe so native
/// commands cannot consume protocol input.
///
/// Windows: kernel32 CreatePipe/SetStdHandle.
/// Unix/macOS/Linux: libc pipe/dup2/fcntl (best-effort port; validate on target OS).
/// </summary>
public static class NativeConsole
{
    public static void Start(Action<string> onOutputLine, Action<string> onErrorLine)
    {
#if WINDOWS
        StartWindows(onOutputLine, onErrorLine);
#else
        StartUnix(onOutputLine, onErrorLine);
#endif
    }

#if WINDOWS
    private const int STD_INPUT_HANDLE = -10;
    private const int STD_OUTPUT_HANDLE = -11;
    private const int STD_ERROR_HANDLE = -12;

    [StructLayout(LayoutKind.Sequential)]
    private sealed class Inheritable
    {
        public int Length;
        public IntPtr SecurityDescriptor;
        public bool InheritHandle;
        public Inheritable() { Length = Marshal.SizeOf(typeof(Inheritable)); InheritHandle = true; }
    }

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CreatePipe(out IntPtr readHandle, out IntPtr writeHandle, Inheritable securityAttributes, int size);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool SetStdHandle(int which, IntPtr handle);
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool CloseHandle(IntPtr handle);

    private static IntPtr NewPipe(out IntPtr readHandle)
    {
        if (!CreatePipe(out readHandle, out IntPtr writeHandle, new Inheritable(), 64 * 1024))
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());
        return writeHandle;
    }

    private static void StartWindows(Action<string> onOutputLine, Action<string> onErrorLine)
    {
        IntPtr stdout = NewPipe(out IntPtr outRead);
        IntPtr stderr = NewPipe(out IntPtr errRead);
        IntPtr stdinWriter = NewPipe(out IntPtr stdin);
        CloseHandle(stdinWriter);

        if (!SetStdHandle(STD_OUTPUT_HANDLE, stdout) || !SetStdHandle(STD_ERROR_HANDLE, stderr) || !SetStdHandle(STD_INPUT_HANDLE, stdin))
            throw new System.ComponentModel.Win32Exception(Marshal.GetLastWin32Error());

        Pump(outRead, onOutputLine, "PS7-ISE-NativeOut");
        Pump(errRead, onErrorLine, "PS7-ISE-NativeErr");
    }
#else
    [DllImport("libc", SetLastError = true)]
    private static extern int pipe(int[] fds);
    [DllImport("libc", SetLastError = true)]
    private static extern int dup2(int oldfd, int newfd);
    [DllImport("libc", SetLastError = true)]
    private static extern int close(int fd);
    [DllImport("libc", SetLastError = true)]
    private static extern int fcntl(int fd, int cmd, int arg);
    private const int F_GETFD = 1;
    private const int F_SETFD = 2;
    private const int FD_CLOEXEC = 1;

    private static void ClearCloexec(int fd)
    {
        int flags = fcntl(fd, F_GETFD, 0);
        if (flags >= 0) fcntl(fd, F_SETFD, flags & ~FD_CLOEXEC);
    }

    private static (int readFd, int writeFd) NewUnixPipe()
    {
        int[] fds = new int[2];
        if (pipe(fds) != 0) throw new IOException("pipe() failed: " + Marshal.GetLastPInvokeError());
        return (fds[0], fds[1]);
    }

    private static void StartUnix(Action<string> onOutputLine, Action<string> onErrorLine)
    {
        var (outRead, outWrite) = NewUnixPipe();
        var (errRead, errWrite) = NewUnixPipe();
        var (inRead, inWrite) = NewUnixPipe();

        // Child should inherit write ends for stdout/stderr and read end for stdin.
        ClearCloexec(outWrite);
        ClearCloexec(errWrite);
        ClearCloexec(inRead);

        if (dup2(outWrite, 1) < 0) throw new IOException("dup2 stdout failed: " + Marshal.GetLastPInvokeError());
        if (dup2(errWrite, 2) < 0) throw new IOException("dup2 stderr failed: " + Marshal.GetLastPInvokeError());
        if (dup2(inRead, 0) < 0) throw new IOException("dup2 stdin failed: " + Marshal.GetLastPInvokeError());

        // Close duplicated/original child-side fds in parent; keep read ends for pumps.
        close(outWrite);
        close(errWrite);
        close(inRead);
        // Close stdin writer so native commands see EOF instead of hanging.
        close(inWrite);

        PumpUnix(outRead, onOutputLine, "PS7-ISE-NativeOut");
        PumpUnix(errRead, onErrorLine, "PS7-ISE-NativeErr");
    }

    private static void PumpUnix(int readFd, Action<string> onLine, string name)
    {
        // Duplicate fd into a SafeFileHandle-owned stream for managed reading.
        int dupFd = dup(readFd);
        if (dupFd < 0) throw new IOException("dup() failed: " + Marshal.GetLastPInvokeError());
        close(readFd);
        var handle = new SafeFileHandle((IntPtr)dupFd, ownsHandle: true);
        var stream = new FileStream(handle, FileAccess.Read);
        var pump = new NativeOutputPump(stream, onLine);
        StartPumpThread(pump, name);
    }

    [DllImport("libc", SetLastError = true)]
    private static extern int dup(int fd);
#endif

    private static void Pump(IntPtr readHandle, Action<string> onLine, string name)
    {
        var pump = new NativeOutputPump(readHandle, onLine);
        StartPumpThread(pump, name);
    }

    private static void StartPumpThread(NativeOutputPump pump, string name)
    {
        Thread thread = new Thread(() =>
        {
            try { pump.Run(); }
            catch (Exception ex) { Console.Error.WriteLine(name + ": " + ex.Message); }
        })
        { IsBackground = true, Name = name };
        thread.Start();
    }
}
