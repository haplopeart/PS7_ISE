using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

/// <summary>
/// Turns one pipe of raw bytes from a native program into whole lines of text.
///
/// The encoding cannot be known in advance: a Windows console program writes whatever its
/// output code page is, which for <c>ipconfig</c> and <c>cmd</c> is OEM, while <c>DISM</c> and
/// anything built on .NET write UTF-8, and one program can even do both in a single run. So
/// each line is tried as UTF-8 and, when the bytes cannot mean that, read as the console's OEM
/// code page. Deciding per line rather than once per stream is what allows the mix: plain ASCII
/// is valid UTF-8 and decodes the same either way, so guessing early costs nothing.
///
/// Then the carriage returns. A progress bar (<c>MpCmdRun.exe -SignatureUpdate</c>, DISM's
/// percentage) ends its updates with a bare CR because a real console overwrites one line.
/// Nothing overwrites here, so CR counts as a line ending too: the steps appear one under the
/// other instead of freezing until the program finally emits an LF.
/// </summary>
internal sealed class NativeOutputPump
{
    private readonly Stream _source;
    private readonly Action<string> _onLine;
    private readonly byte[] _chunk = new byte[8 * 1024];
    private readonly List<byte> _line = new();
    private readonly Encoding _oem;

    [DllImport("kernel32.dll")]
    private static extern uint GetOEMCP();

    public NativeOutputPump(IntPtr readHandle, Action<string> onLine)
    {
        _source = new FileStream(
            new Microsoft.Win32.SafeHandles.SafeFileHandle(readHandle, true), FileAccess.Read);
        _oem = Oem();
        _onLine = onLine;
    }

    /// <summary>Reads until the pipe closes. Intended for a thread of its own.</summary>
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
                        // A CRLF must not then produce an empty second line.
                        if (_line.Count > 0) { Emit(); }
                    }
                    else
                    {
                        _line.Add(b);
                    }
                }
            }
        }
        catch (IOException)
        {
            // The write end went away: the engine is closing, the normal way to end.
        }
        catch (ObjectDisposedException)
        {
        }

        if (_line.Count > 0) { Emit(); }
    }

    /// <summary>Decodes one accumulated line and hands it over.</summary>
    /// <remarks>
    /// Newlines are found by scanning bytes rather than characters, which is safe: 0x0D and 0x0A
    /// cannot occur inside a multi-byte UTF-8 sequence, nor in any single-byte code page.
    /// </remarks>
    private void Emit()
    {
        byte[] bytes = _line.ToArray();
        _line.Clear();
        if (bytes.Length == 0) { return; }

        Encoding encoding = LooksLikeUtf8(bytes) ? Encoding.UTF8 : _oem;
        // A tool that flushes per write may repeat its BOM; it is never content.
        string text = encoding.GetString(bytes).Trim('\uFEFF');
        if (text.Length > 0) { _onLine(text); }
    }

    /// <summary>
    /// True when the bytes can be read as UTF-8. Strict decoding is what makes that a test and
    /// not a guess: OEM accented letters are invalid sequences in nearly every case.
    /// </summary>
    private static bool LooksLikeUtf8(byte[] bytes)
    {
        try
        {
            new UTF8Encoding(encoderShouldEmitUTF8Identifier: false,
                throwOnInvalidBytes: true).GetChars(bytes);
            return true;
        }
        catch (DecoderFallbackException)
        {
            return false;
        }
    }

    /// <summary>
    /// The console's OEM code page, which is what <c>ipconfig</c>, <c>cmd</c> and <c>diskpart</c>
    /// write. Legacy code pages are not guaranteed to be registered, so falling back to UTF-8
    /// keeps a machine without them readable instead of throwing on the first native command.
    /// </summary>
    private static Encoding Oem()
    {
        try
        {
            return Encoding.GetEncoding((int)GetOEMCP());
        }
        catch (Exception)
        {
            return Encoding.UTF8;
        }
    }
}
