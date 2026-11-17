using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Management.Automation;
using System.Management.Automation.Host;
using System.Management.Automation.Language;
using System.Management.Automation.Runspaces;
using System.Text;
using System.Threading;

namespace PowerShellEngineHost;

/// <summary>
/// The interactive PowerShell session: a runspace running on its own thread, an input
/// queue filled by the front-end and an output queue drained by the front-end.
/// This is the same shape the real ISE console uses (prompt -> read line -> invoke).
/// </summary>
public sealed class Session
{
    private const int MaxChunk = 1600;

    private readonly ConcurrentQueue<OutputEvent> _events = new();
    private readonly BlockingCollection<string> _input = new(new ConcurrentQueue<string>());
    private readonly ConcurrentQueue<char> _keys = new();
    private readonly List<string> _history = new();
    private readonly StringBuilder _pending = new();
    private readonly CommandMetadata _metadata = new();

    private Runspace _runspace;
    private ISEHost _host;
    private Thread _worker;
    private PowerShell _running;

    /// <summary>
    /// Guards the runspace, because only one thread may have a pipeline on it - or read its
    /// session state - at a time. The prompt loop holds it for as long as a command runs; the
    /// protocol thread takes it, without waiting, to answer a Tab completion or a command
    /// metadata request, and says "busy" when it cannot have it. A submitted command is the one
    /// thing that waits for it, which is the direction that matters: a panel request can never
    /// be standing on the runspace when a command starts. Asking the same runspace a second
    /// question while it is executing is not merely untidy - it is the kind of thing that ends
    /// the process rather than the pipeline.
    /// </summary>
    private readonly object _runspaceGate = new();

    /// <summary>
    /// How long a submitted command waits for the runspace while a command list is being read.
    /// A command cannot overtake a list that is already being written - both are handled by the
    /// thread that reads the front end's commands, so the line simply waits its turn - which
    /// makes this the bound on how long the prompt can appear to do nothing. Reading the list is
    /// a second the first time and a twentieth afterwards, so two seconds is generous, and a
    /// read wedged longer than that is a reason to say so rather than to wait forever. The
    /// protocol thread can also be holding the gate for a Tab completion, which is the same
    /// trade: it is one line waiting for another line, and bounded either way.
    /// </summary>
    private static readonly TimeSpan GateWait = TimeSpan.FromSeconds(2);

    private int _exitCode;
    private volatile bool _exit;
    private volatile bool _inputRequested;
    private volatile bool _busy;

    /// <summary>The prompt the loop last printed, kept so it can be printed again after the
    /// front end clears the console pane.</summary>
    private volatile string _lastPrompt = string.Empty;

    /// <summary>The directory the prompt last showed, for the moment the runspace is busy.</summary>
    private volatile string _lastCwd = string.Empty;

    /// <summary>Signaled once the prompt loop is over, whatever ended it.</summary>
    public System.Threading.ManualResetEventSlim Closed { get; } = new System.Threading.ManualResetEventSlim(false);

    public int ExitCode => _exitCode;
    public bool IsBusy => _busy;
    public bool InputRequested => _inputRequested;
    public bool Ended => _exit;

    /// <summary>True when ReadKey can return a keystroke without blocking.</summary>
    public bool KeyAvailable => !_keys.IsEmpty;

    /// <summary>
    /// Tab completion, via the same TabExpansion2 machinery the real console uses. It reads the
    /// same runspace the prompt loop is using, so it holds the gate for as long as it takes -
    /// which can be a while, because completing a word can autoload a module.
    /// </summary>
    public IEnumerable<CompletionResult> GetCompletions(string input, int caret)
    {
        if (_runspace == null || _busy) return Array.Empty<CompletionResult>();
        if (!Monitor.TryEnter(_runspaceGate, TimeSpan.Zero)) return Array.Empty<CompletionResult>();

        PowerShell ps = null;
        try
        {
            if (_busy || _runspace == null) return Array.Empty<CompletionResult>();
            caret = Math.Clamp(caret, 0, input.Length);
            ps = PowerShell.Create();
            ps.Runspace = _runspace;
            var completion = CommandCompletion.CompleteInput(input, caret, null, ps);
            if (completion?.CompletionMatches != null) return completion.CompletionMatches;
            return Array.Empty<CompletionResult>();
        }
        catch
        {
            return Array.Empty<CompletionResult>();
        }
        finally
        {
            ps?.Dispose();
            Monitor.Exit(_runspaceGate);
        }
    }

    /// <summary>
    /// Command metadata for the Commands panel. Given back as <c>null</c> when the session
    /// cannot be asked - a command is running, the runspace is gone, or the gate is held by
    /// something else - which is the one answer the panel needs: ask again when the console says
    /// it is parked. A scan that read nothing is thrown rather than answered, because waiting for
    /// a command that has already finished would send the panel round in circles.
    ///
    /// The fast test is <c>_busy</c>; the gate is what makes it true, because a request can
    /// arrive between that test and the pipeline it is about to create, and the runspace must
    /// never be stood on twice.
    /// </summary>
    public string GetCommandIndex(bool rescan)
    {
        if (_runspace == null || _busy) return null;
        if (!Monitor.TryEnter(_runspaceGate, TimeSpan.Zero)) return null;

        try
        {
            // Asked again on the far side of the gate: a command may have started, and the
            // runspace may be the one that has just been disposed.
            if (_busy || _runspace == null) return null;
            if (rescan) _metadata.Invalidate();
            return _metadata.BuildIndex(_runspace);
        }
        finally
        {
            Monitor.Exit(_runspaceGate);
        }
    }

    /// <summary>
    /// Detail for one command, or <c>null</c> when the session cannot answer right now. Throws
    /// when the name is not a command, which is how the front end is told "no such command"
    /// rather than being handed an empty pane.
    /// </summary>
    public string GetCommandDetail(string name)
    {
        if (_runspace == null || _busy) return null;
        if (!Monitor.TryEnter(_runspaceGate, TimeSpan.Zero)) return null;

        try
        {
            if (_busy || _runspace == null) return null;
            return _metadata.BuildDetail(_runspace, name);
        }
        finally
        {
            Monitor.Exit(_runspaceGate);
        }
    }

    /// <summary>Starts the session and its reader thread.</summary>
    public void Start()
    {
        if (_worker != null) return;

        EnsureModulePath();

        var iss = InitialSessionState.CreateDefault2();
        iss.ExecutionPolicy = Microsoft.PowerShell.ExecutionPolicy.Bypass;
        iss.ApartmentState = ApartmentState.STA;

        _runspace = RunspaceFactory.CreateRunspace(_host = new ISEHost(this), iss);
        _runspace.Open();

        // A rich text control cannot draw ANSI escapes, so ask the engine for plain text.
        InvokeSetup("$PSStyle.OutputRendering = 'PlainText'");
        // Keep the child console programs from painting the GUI with escape codes.
        InvokeSetup("$env:TERM = 'dumb'");

        Emit(EventKinds.Output, ConsoleColor.Gray, ConsoleColor.Black,
            "PowerShell " + GetVariable("$PSVersionTable.PSVersion") + Environment.NewLine);
        Emit(EventKinds.Output, ConsoleColor.Gray, ConsoleColor.Black,
            "Copyright (C) Microsoft Corporation. All rights reserved." + Environment.NewLine + Environment.NewLine);

        _worker = new Thread(Loop) { IsBackground = true, Name = "PS7-ISE-Session" };
        _worker.Start();
    }

    private void InvokeSetup(string script)
    {
        try
        {
            // The prompt loop's own reads of the runspace wait for the gate rather than giving
            // up on it: a setup script, a version number and a prompt all have to be right, and
            // the protocol thread holding the gate is never waiting for anything in return.
            lock (_runspaceGate)
            {
                using var ps = PowerShell.Create();
                ps.Runspace = _runspace;
                ps.AddScript(script);
                ps.Invoke();
            }
        }
        catch
        {
            // Not fatal: older engines simply do not have $PSStyle.
        }
    }

    /// <summary>
    /// Puts the modules published next to this assembly first and keeps everything else the
    /// machine offers, in the order PowerShell itself would use.
    ///
    /// The Windows PowerShell 5.1 locations used to be dropped here on the theory that a 5.1
    /// module cannot load into PowerShell 7. That is only true of the compiled ones, and the
    /// cost of dropping them was worse than the problem: it hid commands that PowerShell 7
    /// runs perfectly well, because the 5.1 module directories are where Windows keeps its
    /// cmdlets. `Get-WindowsUpdateLog`, for one, comes from the `WindowsUpdate` module in
    /// `C:\WINDOWS\system32\WindowsPowerShell\v1.0\Modules` and from nowhere else, so the
    /// script a user writes in the original ISE stopped working here. Microsoft's own pwsh
    /// keeps those entries in `$env:PSModulePath` and simply prefers its own, so this does
    /// the same: ours first, the inherited set after it, nothing removed.
    /// </summary>
    private static void EnsureModulePath()
    {
        string home = Path.TrimEndingDirectorySeparator(AppContext.BaseDirectory);
        var parts = new List<string>();

        void Add(string path)
        {
            if (string.IsNullOrWhiteSpace(path) || !Directory.Exists(path)) return;
            string full = Path.TrimEndingDirectorySeparator(path);
            foreach (string seen in parts)
            {
                if (string.Equals(seen, full, StringComparison.OrdinalIgnoreCase)) return;
            }
            parts.Add(full);
        }

        Add(Path.Combine(home, "Modules"));
        Add(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.MyDocuments), "PowerShell", "Modules"));
        Add(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "PowerShell", "Modules"));

        string inherited = Environment.GetEnvironmentVariable("PSModulePath") ?? string.Empty;
        foreach (string entry in inherited.Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries))
        {
            Add(entry);
        }

        if (parts.Count > 0)
        {
            Environment.SetEnvironmentVariable("PSModulePath",
                string.Join(Path.PathSeparator.ToString(), parts));
        }
    }

    private string GetVariable(string expr)
    {
        try
        {
            lock (_runspaceGate)
            {
                using var ps = PowerShell.Create();
                ps.Runspace = _runspace;
                ps.AddScript(expr);
                var r = ps.Invoke();
                return r.Count > 0 ? r[0].ToString() : "?";
            }
        }
        catch
        {
            return "?";
        }
    }

    // ---------------------------------------------------------------- main loop

    private void Loop()
    {
        try
        {
            while (!_exit)
            {
                string promptText = _pending.Length > 0 ? ">> " : GetPrompt();
                if (_exit) break;
                _lastPrompt = promptText;
                Emit(EventKinds.Output, ConsoleColor.Gray, ConsoleColor.Black, promptText);

                RequestInput();
                string line;
                try
                {
                    line = _input.Take();
                }
                catch (InvalidOperationException)
                {
                    break;
                }
                if (line == null || _exit) break;

                _inputRequested = false;

                if (!string.IsNullOrWhiteSpace(line) && (_history.Count == 0 || _history[^1] != line))
                    _history.Add(line);

                // "exit" has to be handled here: it does not tear the runspace down.
                if (_pending.Length == 0 && IsExitCommand(line, out int code))
                {
                    _exitCode = code;
                    _exit = true;
                    break;
                }

                _pending.Append(line).Append("\r\n");
                string script = _pending.ToString();

                if (!IsComplete(script)) continue;

                _pending.Clear();
                if (string.IsNullOrWhiteSpace(script)) continue;

                Execute(script);
            }
        }
        catch (Exception ex)
        {
            Emit(EventKinds.Error, ConsoleColor.Red, ConsoleColor.Black, "Host error: " + ex.Message + Environment.NewLine);
        }
        finally
        {
            // Once the shell is done the front-end must stop offering an input line.
            if (!_exit) RequestInput();
            Closed.Set();
        }
    }

    private static bool IsExitCommand(string line, out int code)
    {
        code = 0;
        var trimmed = line.Trim();
        if (trimmed.Length == 0) return false;

        const string exitWord = "exit";
        if (!trimmed.StartsWith(exitWord, StringComparison.OrdinalIgnoreCase)) return false;
        if (trimmed.Length > exitWord.Length && !char.IsWhiteSpace(trimmed[exitWord.Length]) && trimmed[exitWord.Length] != ';')
            return false;

        var rest = trimmed.Substring(exitWord.Length).Trim().TrimEnd(';').Trim();
        if (rest.Length == 0) return true;
        return int.TryParse(rest, out code);
    }

    /// <summary>
    /// Parse errors that mean "keep reading" rather than "this is broken". The engine
    /// keeps its own IsCompleteInput internal, so the same signals are derived here.
    /// </summary>
    private static readonly HashSet<string> IncompleteIds = new(StringComparer.OrdinalIgnoreCase)
    {
        "MissingEndCurlyBrace",
        "MissingEndParenthesisInExpression",
        "MissingEndParenthesisInSubexpression",
        "MissingEndParenthesisInFunctionParameterList",
        "MissingEndParenthesisAfterStatement",
        "MissingEndParenthesisInMethodCall",
        "MissingArrayIndexExpression",
        "MissingPropertyName",
        "TerminatorExpectedAtEndOfString",
        "IncompleteString",
        "ExpectedExpression",
        "ExpectedValueExpression",
        "EmptyPipeElement",
        "MissingStatementBlock",
        "MissingSwitchConditionExpression",
        "MissingWhileOrUntilInDoWhile",
        "PipelineValueRequired",
        "MissingEndSquareBracket",
        "MissingTypename",
        "MissingAttributeArgument",
    };

    private static bool IsComplete(string script)
    {
        try
        {
            Parser.ParseInput(script, out Token[] _, out ParseError[] errors);

            foreach (var error in errors)
            {
                if (IncompleteIds.Contains(error.ErrorId)) return false;
            }

            // A trailing backtick continues the line even though it parses cleanly.
            string trimmed = script.TrimEnd();
            if (trimmed.EndsWith("`", StringComparison.Ordinal) &&
                !trimmed.EndsWith("``", StringComparison.Ordinal))
            {
                return false;
            }

            return true;
        }
        catch
        {
            return true;
        }
    }

    private string GetPrompt()
    {
        try
        {
            lock (_runspaceGate)
            {
                using var ps = PowerShell.Create();
                ps.Runspace = _runspace;
                ps.AddScript("prompt", useLocalScope: true);
                var results = ps.Invoke();
                if (ps.HadErrors) throw new RuntimeException("prompt failed");

                if (results.Count > 0)
                    return string.Concat(results.Select(static r => r?.ToString() ?? string.Empty));
            }
        }
        catch
        {
            // fall through to the built-in prompt
        }

        string cwd = CurrentLocation();
        return "PS " + cwd + "> ";
    }

    /// <summary>
    /// The directory to print in the prompt. Reading a variable is runspace work, so it goes
    /// through the gate like everything else; the prompt is only ever drawn while the loop is
    /// parked, which means the gate is nearly always free, and the last known directory is a
    /// good enough answer in the one case where it is not.
    /// </summary>
    private string CurrentLocation()
    {
        if (!Monitor.TryEnter(_runspaceGate, TimeSpan.Zero)) return _lastCwd;

        try
        {
            object pwd = _runspace?.SessionStateProxy.GetVariable("PWD");
            string cwd = pwd == null ? _lastCwd : (Convert.ToString(pwd) ?? _lastCwd);
            _lastCwd = cwd;
            return cwd;
        }
        catch
        {
            return _lastCwd;
        }
        finally
        {
            Monitor.Exit(_runspaceGate);
        }
    }

    // ---------------------------------------------------------------- execution

    private void Execute(string script)
    {
        // Claiming the runspace before taking the gate, so a metadata request that is between
        // its "is the session free" test and its own pipeline sees this and steps back.
        _busy = true;
        // The front-end echoes the newline for the submitted line, so these boundary
        // events carry state only.
        Emit(EventKinds.CommandStarted, ConsoleColor.Gray, ConsoleColor.Black, string.Empty);

        // Waiting rather than testing. A submitted command cannot overtake a command list that
        // is being written - the same thread handles both - so this is the wait that makes, and
        // bounds, that queueing: reading the list is a second at the outside, and past the bound
        // the line says so in the console instead of being swallowed. It cannot deadlock: the
        // protocol thread waits for nothing here, it only gives way.
        bool held = Monitor.TryEnter(_runspaceGate, GateWait);

        if (held)
        {
            try
            {
                Run(script);
            }
            finally
            {
                Monitor.Exit(_runspaceGate);
            }
        }
        else
        {
            Emit(EventKinds.Error, ConsoleColor.Red, ConsoleColor.Black,
                "The command list is still being read. Try again in a moment." + Environment.NewLine);
        }

        // The gate is back, so a metadata request may have the runspace again from here on.
        _busy = false;
        Emit(EventKinds.CommandFinished, ConsoleColor.Gray, ConsoleColor.Black, string.Empty);
    }

    /// <summary>Runs one submitted line on the runspace, which the caller must be holding.</summary>
    private void Run(string script)
    {
        PowerShell ps = null;
        try
        {
            ps = PowerShell.Create();
            ps.Runspace = _runspace;
            // Out-Default gives us exactly what the console would have shown:
            // formatted tables, colours, Write-Host, warnings, verbose and debug lines.
            ps.AddScript(script);
            ps.AddCommand("Out-Default");

            _running = ps;
            ps.Invoke();
            _running = null;

            foreach (var error in ps.Streams.Error)
                Emit(EventKinds.Error, ConsoleColor.Red, ConsoleColor.Black, FormatError(error));

            // Clear so a reused pipeline does not replay old records.
            ps.Streams.Error.Clear();
        }
        catch (PipelineStoppedException)
        {
            Emit(EventKinds.Output, ConsoleColor.DarkYellow, ConsoleColor.Black,
                "Stop-Processing: the command was stopped by the user." + Environment.NewLine);
        }
        catch (Exception ex)
        {
            Emit(EventKinds.Error, ConsoleColor.Red, ConsoleColor.Black, ex.Message + Environment.NewLine);
        }
        finally
        {
            // Let go of the pipeline before disposing it: PowerShell's own Dispose insists the
            // state has already left Executing, and Stop() may be reaching for the same object
            // from the protocol thread. _busy is the caller's to clear, once the gate is back.
            _running = null;
            ps?.Dispose();
        }
    }

    private string FormatError(ErrorRecord error)
    {
        try
        {
            using var ps = PowerShell.Create();
            ps.Runspace = _runspace;
            ps.AddCommand("Out-String").AddParameter("Width", 120);
            var results = ps.Invoke(new object[] { error });
            var text = string.Concat(results.Select(static r => r?.ToString() ?? string.Empty));
            if (!string.IsNullOrWhiteSpace(text)) return StripAnsi(text);
        }
        catch
        {
            // fall through
        }
        return StripAnsi(error.ToString()) + Environment.NewLine;
    }

    /// <summary>
    /// Stops the running command (Ctrl+C equivalent). Safe from any thread. It gives up a
    /// command list too: from the front end the two cannot overlap, since the thread answering a
    /// list is not reading the interrupt, but shutting down goes through here first, and that is
    /// where a wedged read is worth stopping - the runspace is about to be disposed.
    /// </summary>
    public void Stop()
    {
        _metadata.Cancel();
        try
        {
            _running?.Stop();
        }
        catch
        {
            // ignore: the pipeline may already be finished
        }
    }

    /// <summary>
    /// Prints the prompt again after the front end wiped the console pane. Without this a
    /// cleared pane stays empty and the user types into blank space. Only valid while the
    /// loop is actually sitting at that prompt, which is also what makes it safe to read
    /// the prompt text from another thread: no pipeline is running at that point.
    /// </summary>
    public void RedrawPrompt()
    {
        if (!_inputRequested) return;
        Emit(EventKinds.Output, ConsoleColor.Gray, ConsoleColor.Black, _lastPrompt);
    }

    /// <summary>Queues a line typed by the user.</summary>
    public bool SubmitLine(string line)
    {
        if (_exit) return false;
        foreach (char c in line ?? string.Empty) _keys.Enqueue(c);
        _input.Add(line ?? string.Empty);
        return true;
    }

    public void RequestExit(int code)
    {
        _exitCode = code;
        _exit = true;
        try { _input.Add(string.Empty); } catch { }
        Stop();
        Closed.Set();
    }

    public void Shutdown()
    {
        _exit = true;
        try { _input.Add(string.Empty); } catch { }
        Stop();
        try { _input.CompleteAdding(); } catch { }
        try { _worker?.Join(TimeSpan.FromSeconds(2)); } catch { }

        // Stop() above has already asked a command list to give up, so the wait is for a read
        // that is on its way out. It is worth waiting: disposing a runspace from under a
        // pipeline that is using it is the one thing the gate exists to prevent, and this is
        // the only place the runspace leaves.
        bool held = Monitor.TryEnter(_runspaceGate, GateWait);
        try
        {
            _runspace?.Dispose();
        }
        catch
        {
            // A runspace that will not close is not worth the exit over.
        }
        finally
        {
            _runspace = null;
            if (held) Monitor.Exit(_runspaceGate);
        }

        Closed.Set();
    }

    // ---------------------------------------------------------------- input side

    /// <summary>Called by PSHostUserInterface.ReadLine (Read-Host and friends).</summary>
    public string ReadLine()
    {
        RequestInput();
        string line = _input.Take();
        _inputRequested = false;
        return line ?? string.Empty;
    }

    public KeyInfo ReadKey(bool includeKeyDown)
    {
        if (_keys.Count == 0)
        {
            RequestInput();
            string line = _input.Take();
            _inputRequested = false;
            foreach (char c in (line ?? string.Empty)) _keys.Enqueue(c);
            _keys.Enqueue('\r');
        }
        _keys.TryDequeue(out char ch);
        return new KeyInfo(VirtualKeyFor(ch), ch, (ControlKeyStates)0, includeKeyDown);
    }

    /// <summary>Maps a character to the virtual key code a console would report.</summary>
    private static int VirtualKeyFor(char ch)
    {
        if (ch >= 'a' && ch <= 'z') return ch - 'a' + 'A';
        if (ch >= 'A' && ch <= 'Z') return ch;
        if (ch >= '0' && ch <= '9') return ch;
        if (ch == '\r' || ch == '\n') return 0x0D;
        if (ch == '\t') return 0x09;
        if (ch == '\x1b') return 0x1B;
        return 0;
    }

    public void FlushInput()
    {
        while (_input.TryTake(out _)) { }
        while (_keys.TryDequeue(out _)) { }
    }

    private void RequestInput()
    {
        _inputRequested = true;
        Emit(EventKinds.InputRequested, ConsoleColor.Gray, ConsoleColor.Black, CurrentLocation());
    }

    // ---------------------------------------------------------------- output side

    /// <summary>Adds an event to the queue the front-end drains, split into printable chunks.</summary>
    public void Emit(int kind, ConsoleColor fg, ConsoleColor bg, string text)
    {
        text = StripAnsi(text ?? string.Empty);
        if (text.Length == 0 && kind == EventKinds.Output) return;

        for (int i = 0; i < text.Length; i += MaxChunk)
        {
            int len = Math.Min(MaxChunk, text.Length - i);
            _events.Enqueue(new OutputEvent(kind, fg, bg, text.Substring(i, len)));
            // Only the first chunk carries non-text metadata.
            if (kind == EventKinds.InputRequested || kind == EventKinds.Title) kind = EventKinds.Output;
        }
        if (text.Length == 0) _events.Enqueue(new OutputEvent(kind, fg, bg, text));
    }

    /// <summary>Removes ANSI escape sequences; the console pane is rich text, not a terminal.</summary>
    private static string StripAnsi(string s)
    {
        if (s.Length == 0 || s.IndexOf('\u001b') < 0) return s;

        var sb = new StringBuilder(s.Length);
        for (int i = 0; i < s.Length; i++)
        {
            char c = s[i];
            if (c != '\u001b' || i + 1 >= s.Length)
            {
                sb.Append(c);
                continue;
            }

            char marker = s[i + 1];
            if (marker != '[' && marker != ']')
            {
                sb.Append(c);
                continue;
            }

            // Scan from the byte after the introducer; the introducer is never a terminator.
            int j = i + 2;
            while (j < s.Length)
            {
                char t = s[j];
                bool terminated = marker == '['
                    ? t >= '@' && t <= '~'
                    : t == '\a' || t == '\u001b';
                if (terminated) break;
                j++;
            }
            i = j; // the loop's ++ steps over the terminator
        }
        return sb.ToString();
    }

    /// <summary>Pops the next pending event, or null when nothing is waiting.</summary>
    public OutputEvent NextEvent()
        => _events.TryDequeue(out var e) ? e : null;

    public IReadOnlyList<string> History => _history;
}
