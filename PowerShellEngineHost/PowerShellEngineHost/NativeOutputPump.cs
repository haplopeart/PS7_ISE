using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

/// <summary>
/// Turns one pipe of raw bytes from a native program into whole lines of text.
///
/// Encoding is platform-aware: on Windows we try UTF-8 and fall back to the console OEM code page;
/// on Unix/macOS/Linux we default to UTF-8 with tolerant decoding.
/// </summary>
internal sealed class NativeOutputPump
{
    private readonly Stream _source;
    private readonly Action<string> _onLine;
    private readonly byte[] _chunk = new byte[8 * 1024];
    private readonly List<byte> _line = new();
    private readonly Encoding _fallback;

#if WINDOWS
    [DllImport("kernel32.dll")]
    private static extern uint GetOEMCP();
#endif

    public NativeOutputPump(IntPtr readHandle, Action<string> onLine)
    {
        _source = new FileStream(
            new Microsoft.Win32.SafeHandles.SafeFileHandle(readHandle, true), FileAccess.Read);
        _fallback = GetFallbackEncoding();
        _onLine = onLine;
    }

    public NativeOutputPump(Stream source, Action<string> onLine)
    {
        _source = source;
        _fallback = GetFallbackEncoding();
        _onLine = onLine;
    }

    public void Run()
    {
        try
        {
            int read;
            while ((read = _source.Read(_chunk, 0, _chunk.Length)) > 0)
            {
                for (int i = 0; i < read; i++)
                {
                    byte b = _chunk[i];
                    if (b == (byte)'\n')
                    {
                        Emit();
                    }
                    else if (b == (byte)'\r')
                    {
                        if (_line.Count > 0) { Emit(); }
                    }
                    else
                    {
                        _line.Add(b);
                    }
                }
            }
        }
        catch (IOException) { }
        catch (ObjectDisposedException) { }

        if (_line.Count > 0) { Emit(); }
    }

    private void Emit()
    {
        byte[] bytes = _line.ToArray();
        _line.Clear();
        if (bytes.Length == 0) { return; }

        Encoding encoding = LooksLikeUtf8(bytes) ? Encoding.UTF8 : _fallback;
        string text = encoding.GetString(bytes).Trim('\uFEFF');
        if (text.Length > 0) { _onLine(text); }
    }

    private static bool LooksLikeUtf8(byte[] bytes)
    {
        try
        {
            new UTF8Encoding(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true).GetChars(bytes);
            return true;
        }
        catch (DecoderFallbackException)
        {
            return false;
        }
    }

    private static Encoding GetFallbackEncoding()
    {
#if WINDOWS
        try { return Encoding.GetEncoding((int)GetOEMCP()); }
        catch { return Encoding.UTF8; }
#else
        return new UTF8Encoding(false, false);
#endif
    }
}
