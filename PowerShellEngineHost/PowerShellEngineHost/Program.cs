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
///   M &lt;what&gt;    command metadata for the Commands panel: "i" lists every command once,
///           "r" lists them again after a rescan, "d &lt;name&gt;" describes one command
///   S         interrupt the running command (Ctrl+C)
///   P         print the prompt again (the front end cleared the console pane)
///   X         shut the session down
///
/// Replies    (engine -> front-end, one per line)
///   E &lt;kind&gt; &lt;fg&gt; &lt;bg&gt; &lt;text&gt;   one coloured run of console text
///   C &lt;count&gt; &lt;text&gt;        completions, separated by U+001E
///   M &lt;what&gt; &lt;part&gt; &lt;of&gt; &lt;reply&gt; &lt;text&gt;  one chunk of a metadata reply. &lt;what&gt; is "i" for
///           the index, "d" for one command, "e" for a request that could not be answered;
///           parts are numbered from 1 to &lt;of&gt;, and there is always at least one, so a
///           reply is whole exactly when the parts seen reach &lt;of&gt;. &lt;reply&gt; counts
///           up with every answer, because a front end that has given up on one request
///           needs to be able to tell its parts from the next request's.
///   Q &lt;code&gt;          the session is over
///
/// Every &lt;text&gt; field is escaped so that a record never contains a line break: only
/// printable ASCII survives literally, everything else becomes \uXXXX, and a backslash
/// is written as \\.
///
/// A metadata reply is chunked because the front end reads records on a thread that holds
/// the lock its user interface takes every fortieth of a second: one record carrying four
/// hundred kilobytes of command list would stop that interface for as long as it takes to
/// decode, where any number of bounded records cannot.
///
/// Console records and metadata parts come from different threads, so they interleave: a
/// front end collecting a metadata reply will find console output between its parts, and has
/// to keep the parts it has rather than treat that as the reply having failed.
/// </summary>
public static class Program
{
    /// <summary>Separates completion items inside one record.</summary>
    private const char ItemSeparator = '\u001e';

    /// <summary>
    /// How much metadata one record carries. It has to be far smaller than a command list and
    /// far larger than a record worth of overhead: the front end decodes each record while
    /// holding the lock its UI thread takes every fortieth of a second, so this is the length
    /// of the pause the interface can be made to take, and four dozen records for a whole
    /// index is nothing against one record for each command.
    /// </summary>
    private const int MetadataChunk = 48 * 1024;

    /// <summary>A longer failure is not a more useful one, it is only a heavier reply.</summary>
    private const int MaxErrorText = 4000;

    /// <summary>Counts metadata replies, so a front end can tell one answer's parts from
    /// another's. Shared between the thread that reads commands and nothing else, but counted
    /// safely anyway, because a reply that is sure of its own number is worth the one call.</summary>
    private static int _metadataReply;

    /// <summary>
    /// Said when the runspace is busy. The scan is a second of work and the panel asks for it
    /// while the user is looking at it, so it is served only at the prompt - which is also
    /// what keeps it from being queued ahead of a Ctrl+C the user is waiting on.
    /// </summary>
    private const string BusyMessage =
        "The console is running a command. The command list is ready as soon as it finishes.";

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

        // Asked before the runspace opens so the notice lands above the first prompt.
        // $PSHome cannot be redirected from here - PowerShell keeps it read-only and points it
        // at this engine's own folder - so all this can do is tell the user whether jobs will
        // run rather than try to arrange it.
        string jobNotice = JobHost.Ensure();
        if (!string.IsNullOrEmpty(jobNotice))
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

                        case 'M':
                            WriteCommandMetadata(session, argument);
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

    /// <summary>
    /// Answers a metadata request. Whatever happens a reply goes back: the front end is
    /// waiting on the part count, and a request that produced nothing would leave the
    /// Commands panel spinning. A failure is therefore an "e" reply that says what went
    /// wrong, not silence.
    /// </summary>
    private static void WriteCommandMetadata(Session session, string argument)
    {
        string kind;
        string payload;

        try
        {
            if (argument == "i" || argument == "r")
            {
                string index = session.GetCommandIndex(argument == "r");
                if (index == null)
                {
                    kind = "e";
                    payload = BusyMessage;
                }
                else
                {
                    kind = "i";
                    payload = index;
                }
            }
            else if (argument.StartsWith("d", StringComparison.Ordinal))
            {
                string name = argument.Length > 2 ? Decode(argument.Substring(2)) : string.Empty;
                string detail = session.GetCommandDetail(name);
                if (detail == null)
                {
                    kind = "e";
                    payload = BusyMessage;
                }
                else
                {
                    kind = "d";
                    payload = detail;
                }
            }
            else
            {
                kind = "e";
                payload = "Unknown metadata request '" + argument + "'.";
            }
        }
        catch (Exception ex)
        {
            // Get-Command and Get-Help throw for a name that is not a command, which is the
            // answer the panel needs to show, so it is passed through rather than swallowed.
            // Some wrappers throw with nothing to say, and an empty explanation reads in the
            // pane as if the panel itself had failed, so the kind of failure stands in for it.
            kind = "e";
            string message = ex.Message;
            payload = string.IsNullOrWhiteSpace(message) ? "The engine could not read that command: " + ex.GetType().Name + "." : message;
        }

        // Only a failure is kept short: a longer explanation is not a more useful one, it is
        // just a heavier reply. Metadata is not trimmed at all - a command list is as long as
        // it is, and the chunking below is what keeps it off the front end's UI thread.
        if (kind == "e" && payload.Length > MaxErrorText) payload = payload.Substring(0, MaxErrorText);

        // Counted up with every answer, including the ones that failed. The front end asks for
        // the list again the moment something else changes - a second selection, a refresh -
        // and a part of the answer it has already given up on has to be recognisable as such,
        // or two replies merge into a list that matches neither.
        int reply = Interlocked.Increment(ref _metadataReply);

        int total = Math.Max(1, (payload.Length + MetadataChunk - 1) / MetadataChunk);
        for (int i = 0, part = 1; part <= total; part++)
        {
            int take = Math.Min(MetadataChunk, Math.Max(0, payload.Length - i));

            // Chopping a chunk anywhere would be wrong in exactly one place: between the two
            // halves of a surrogate pair, which would hand the front end a character it cannot
            // represent. A chunk loses its last character to the next one instead - but never
            // down to nothing, or the part would be empty and everything after it unreachable.
            if (take > 1 && i + take < payload.Length && char.IsHighSurrogate(payload[i + take - 1])) take--;

            string piece = payload.Substring(i, take);
            i += take;
            WriteRaw(string.Concat(
                "M ", kind, " ",
                part.ToString(CultureInfo.InvariantCulture), " ",
                total.ToString(CultureInfo.InvariantCulture), " ",
                reply.ToString(CultureInfo.InvariantCulture), " ",
                Encode(piece)));
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
