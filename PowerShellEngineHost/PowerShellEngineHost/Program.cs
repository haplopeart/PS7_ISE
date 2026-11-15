using System;
using System.Globalization;
using System.IO;
using System.Text;
using System.Threading;

namespace PowerShellEngineHost;

/// <summary>
/// Entry point of the engine process. PS7_ISE.exe starts this executable, writes single
/// line commands to its standard input and reads single line events from its standard
/// output, so PowerShell 7 runs next to the editor exactly like the console pane of the
/// original ISE, while a crash in the engine cannot take the window down.
///
/// Commands   (front-end -> engine, one per line)
///   L &lt;text&gt;   a line was submitted at the prompt
///   T &lt;caret&gt; &lt;text&gt;    ask for TabExpansion2 completions
///   S         interrupt the running command (Ctrl+C)
///   P         print the prompt again (the front end cleared the console pane)
///   X         shut the session down
///
/// Replies    (engine -> front-end, one per line)
///   E &lt;kind&gt; &lt;fg&gt; &lt;bg&gt; &lt;text&gt;   one coloured run of console text
///   C &lt;count&gt; &lt;text&gt;        completions, separated by U+001E
///   Q &lt;code&gt;          the session is over
///
/// Every &lt;text&gt; field is escaped so that a record never contains a line break: only
/// printable ASCII survives literally, everything else becomes \uXXXX, and a backslash
/// is written as \\.
/// </summary>
public static class Program
{
    /// <summary>Separates completion items inside one record.</summary>
    private const char ItemSeparator = '\u001e';

    private static TextWriter _protocol = Console.Out;
    private static readonly object _gate = new();

    public static int Main()
    {
        // Standard output carries the protocol. Anything PowerShell or a native program
        // writes straight to the console is diverted to standard error so that a stray
        // write cannot split a record in half.
        var protocol = new StreamWriter(Console.OpenStandardOutput(), new UTF8Encoding(false)) { AutoFlush = true };
        var diagnostic = new StreamWriter(Console.OpenStandardError(), new UTF8Encoding(false)) { AutoFlush = true };
        _protocol = protocol;
        try
        {
            Console.SetOut(diagnostic);
            Console.SetError(diagnostic);
        }
        catch
        {
            // A host that refuses to redirect simply shares the protocol channel.
        }

        var session = new Session();

        // Opened before the handles move below: once NativeConsole has replaced standard input,
        // asking .NET for it would return the empty pipe meant for native commands and the
        // protocol's own records would never arrive.
        var protocolInput = Console.OpenStandardInput();

        // Native programs are given their standard handles when they are created, so the
        // handles have to be moved before PowerShell exists; a command already running would
        // keep the protocol pipe it was born with. Their text comes back as ordinary console
        // lines - grey for stdout, red for stderr - which is how a real console shows them.
        try
        {
            NativeConsole.Start(
                line => WriteEvent(new OutputEvent(EventKinds.Output, ConsoleColor.Gray,
                    ConsoleColor.Black, line + Environment.NewLine)),
                line => WriteEvent(new OutputEvent(EventKinds.Error, ConsoleColor.Red,
                    ConsoleColor.Black, line + Environment.NewLine)));
        }
        catch (Exception ex)
        {
            // Without the pipes the shell still works; only native text would be lost.
            Console.Error.WriteLine("native output capture unavailable: " + ex.Message);
        }

        var closing = new CancellationTokenSource();
        var pump = new Thread(() => PumpEvents(session, closing.Token))
        {
            IsBackground = false,
            Name = "PS7-ISE-Protocol",
        };
        pump.Start();

        string jobNotice = JobHost.Ensure();
        if (jobNotice != null)
        {
            WriteEvent(new OutputEvent(EventKinds.Output, ConsoleColor.Gray,
                ConsoleColor.Black, jobNotice + Environment.NewLine));
        }

        try
        {
            // Writes the banner and starts the runspace thread; output flows as events.
            session.Start();
        }
        catch (Exception ex)
        {
            WriteEvent(new OutputEvent(EventKinds.Error, ConsoleColor.Red, ConsoleColor.Black,
                "Failed to start PowerShell: " + ex + Environment.NewLine));
            session.Shutdown();
            closing.Cancel();
            pump.Join(TimeSpan.FromSeconds(3));
            WriteRaw("Q 1");
            return 1;
        }

        // Standard input is read on its own thread: typing "exit" ends the session from
        // the runspace side, and Main must not sit blocked in ReadLine when that happens.
        var reader = new Thread(() => ReadCommands(session, protocolInput))
        {
            IsBackground = true,
            Name = "PS7-ISE-Input",
        };
        reader.Start();

        session.Closed.Wait();
        session.Shutdown();

        // Let the pump flush what is still queued before the process goes away.
        closing.Cancel();
        pump.Join(TimeSpan.FromSeconds(3));

        WriteRaw("Q " + session.ExitCode.ToString(CultureInfo.InvariantCulture));
        return session.ExitCode;
    }

    /// <summary>Dispatches front-end commands until the pipe closes.</summary>
    /// <remarks>
    /// The stream is passed in rather than opened here: standard input has since been pointed at
    /// the pipe native commands read from, so the protocol's own handle is only reachable through
    /// what Main kept before that happened.
    /// </remarks>
    private static void ReadCommands(Session session, Stream protocolInput)
    {
        using (var input = new StreamReader(protocolInput, new UTF8Encoding(false)))
        {
            string command;
            while ((command = input.ReadLine()) != null)
            {
                if (command.Length == 0) continue;

                char verb = command[0];
                string argument = command.Length > 2 ? command.Substring(2) : string.Empty;

                try
                {
                    switch (verb)
                    {
                        case 'L':
                            session.SubmitLine(Decode(argument));
                            break;

                        case 'T':
                            WriteCompletions(session, argument);
                            break;

                        case 'S':
                            session.Stop();
                            break;

                        case 'P':
                            session.RedrawPrompt();
                            break;

                        case 'X':
                            session.RequestExit(0);
                            return;

                        default:
                            break;
                    }
                }
                catch (Exception ex)
                {
                    WriteEvent(new OutputEvent(EventKinds.Error, ConsoleColor.Red, ConsoleColor.Black,
                        "Engine error: " + ex.Message + Environment.NewLine));
                }
            }
        }

        // The front-end is gone: take the session with it instead of idling forever.
        session.RequestExit(session.ExitCode);
    }

    /// <summary>Forwards session output to the front-end until the process is closing.</summary>
    private static void PumpEvents(Session session, CancellationToken token)
    {
        while (true)
        {
            OutputEvent next = session.NextEvent();
            if (next != null)
            {
                WriteEvent(next);
                continue;
            }

            if (token.IsCancellationRequested)
            {
                // Final drain: nothing may be lost between here and process exit.
                while ((next = session.NextEvent()) != null) WriteEvent(next);
                return;
            }

            Thread.Sleep(2);
        }
    }

    private static void WriteCompletions(Session session, string argument)
    {
        int space = argument.IndexOf(' ');
        if (space < 0)
        {
            WriteRaw("C 0 ");
            return;
        }

        if (!int.TryParse(argument.Substring(0, space), NumberStyles.Integer, CultureInfo.InvariantCulture, out int caret))
            caret = 0;

        string input = Decode(argument.Substring(space + 1));

        try
        {
            var sb = new StringBuilder();
            int count = 0;
            foreach (var item in session.GetCompletions(input, caret))
            {
                if (count > 0) sb.Append(ItemSeparator);
                sb.Append(item.CompletionText);
                count++;
            }
            WriteRaw("C " + count.ToString(CultureInfo.InvariantCulture) + " " + Encode(sb.ToString()));
        }
        catch
        {
            WriteRaw("C 0 ");
        }
    }

    private static void WriteEvent(OutputEvent item)
    {
        WriteRaw(string.Concat(
            "E ",
            item.Kind.ToString(CultureInfo.InvariantCulture), " ",
            Palette.ToRgb(item.Fg).ToString(CultureInfo.InvariantCulture), " ",
            Palette.ToRgb(item.Bg).ToString(CultureInfo.InvariantCulture), " ",
            Encode(item.Text)));
    }

    private static void WriteRaw(string record)
    {
        lock (_gate)
        {
            try
            {
                _protocol.Write(record);
                _protocol.Write('\n');
                _protocol.Flush();
            }
            catch
            {
                // The front-end closed the pipe: it is going away, so stop talking.
            }
        }
    }

    /// <summary>Turns text into a single printable ASCII field.</summary>
    private static string Encode(string text)
    {
        if (string.IsNullOrEmpty(text)) return string.Empty;

        var sb = new StringBuilder(text.Length + 8);
        foreach (char c in text)
        {
            if (c == '\\')
            {
                sb.Append("\\\\");
            }
            else if (c >= ' ' && c <= '~')
            {
                sb.Append(c);
            }
            else
            {
                sb.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
            }
        }
        return sb.ToString();
    }

    /// <summary>Reverses <see cref="Encode"/>; a malformed tail is dropped rather than thrown.</summary>
    private static string Decode(string text)
    {
        if (string.IsNullOrEmpty(text)) return string.Empty;
        if (text.IndexOf('\\') < 0) return text;

        var sb = new StringBuilder(text.Length);
        for (int i = 0; i < text.Length; i++)
        {
            char c = text[i];
            if (c != '\\' || i + 1 >= text.Length)
            {
                sb.Append(c);
                continue;
            }

            char next = text[i + 1];
            if (next == '\\')
            {
                sb.Append('\\');
                i++;
            }
            else if (next == 'u' && i + 5 < text.Length)
            {
                string hex = text.Substring(i + 2, 4);
                if (int.TryParse(hex, NumberStyles.HexNumber, CultureInfo.InvariantCulture, out int code))
                {
                    sb.Append((char)code);
                    i += 5;
                }
                else
                {
                    sb.Append(c);
                }
            }
            else
            {
                sb.Append(c);
            }
        }
        return sb.ToString();
    }
}
