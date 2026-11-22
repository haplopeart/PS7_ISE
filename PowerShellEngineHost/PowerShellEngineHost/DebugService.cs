using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Globalization;
using System.Management.Automation;
using System.Management.Automation.Runspaces;
using System.Text;
using System.Threading;

namespace PowerShellEngineHost;

/// <summary>
/// The debugger the front-end drives: breakpoints, the stop, stepping, and the variable and
/// call-stack reads that only make sense while execution is held.
/// </summary>
public sealed class DebugService
{
    /// <summary>
    /// Reply number for each debugger protocol request. Counted with Interlocked because a
    /// reply is written both from the runspace thread, answering a request it took out of the
    /// stop loop, and from the protocol thread, refusing a request that arrived with no stop
    /// outstanding. Those two must not hand out the same number, or the front end stitches
    /// half of a refusal onto the end of a variable list.
    /// </summary>
    private int _reply;

    /// <summary>Takes the next reply number.</summary>
    private int NextReply() => Interlocked.Increment(ref _reply);

    /// <summary>
    /// Answers a request that never reached the stop loop, so the pane that asked has
    /// something to show rather than waiting for an answer that is not coming. It is written
    /// as an ordinary reply carrying one error row, which means the front end needs no second
    /// path for a refusal: the same code shows it, in the same place, with the engine's words.
    /// </summary>
    public void Notice(string message) => Program.WriteChunked("D", "e", Rows(RowOf("e", message)), NextReply());

    /// <summary>Held around every debugger call.</summary>
    private readonly object _dbgGate = new();

    /// <summary>Requests waiting for the stop loop.</summary>
    private readonly BlockingCollection<Request> _requests = new(new ConcurrentQueue<Request>());

    /// <summary>True between a DebuggerStop and the resume that ends it.</summary>
    private volatile bool _stopped;

    /// <summary>True once the debugger has been attached to a runspace.</summary>
    public bool Available => _dbg != null;

    /// <summary>One command from the front end, and the resume it may be asking for.</summary>
    private sealed class Request
    {
        public string Verb;
        public string Argument;
    }

    /// <summary>The debugger instance from the runspace.</summary>
    private Debugger _dbg;

    /// <summary>The session, so we can report errors back through the protocol.</summary>
    private Session _session;

    /// <summary>True between DebuggerStop and the resume that ends it.</summary>
    public bool IsStopped => _stopped;

    /// <summary>Separates one row of a payload from the next: the same marks the command list uses.</summary>
    public static readonly char Row = '\u001e';

    /// <summary>Separates the fields inside one row.</summary>
    public static readonly char Field = '\u001f';

    /// <summary>
    /// Hooks the debugger to the session's runspace. Called once, before the first prompt, so
    /// the very first script the user runs is already breakable.
    /// </summary>
    /// <summary>
    /// Binds the debugger that belongs to <paramref name="runspace"/>.
    ///
    /// Called once, when the session's runspace is opened. The stop event is attached here and
    /// never removed: the runspace lives as long as the window does, and a handler that came
    /// off would leave a breakpoint with nowhere to report to, which reads to the user as a
    /// breakpoint that silently stopped working.
    /// </summary>
    public void Attach(Session session, Runspace runspace)
    {
        _session = session;
        _dbg = runspace.Debugger;
        if (_dbg == null) return;

        _dbg.StartRunspaceDebugProcessing += (s, e) => e.UseDefaultProcessing = true;
        _dbg.DebuggerStop += OnDebuggerStop;
    }

    // ------------------------------------------------------------------ mode

    /// <summary>Puts the runspace into script-debugging mode while a breakpoint exists.</summary>
    public void UpdateMode()
    {
        Debugger dbg = _dbg;
        if (dbg == null) return;

        bool any;
        lock (_dbgGate) { any = dbg.GetBreakpoints().Count > 0; }
        try { lock (_dbgGate) { dbg.SetDebugMode(any ? DebugModes.LocalScript | DebugModes.RemoteScript : DebugModes.Default); } }
        catch { }
    }

    // ------------------------------------------------------------------ the stop

    /// <summary>Runs on the runspace thread with execution held, until a resume.</summary>
    private void OnDebuggerStop(object sender, DebuggerStopEventArgs e)
    {
        _stopped = true;
        ReportStopped(e);
        while (true)
        {
            if (_session == null || _session.Ended) { e.ResumeAction = DebuggerResumeAction.Stop; break; }
            if (!_requests.TryTake(out Request req, TimeSpan.FromMilliseconds(100))) continue;
            DebuggerResumeAction? resume = ResumeVerb(req.Verb);
            if (resume.HasValue) { e.ResumeAction = resume.Value; break; }
            try { Handle(req); }
            catch (Exception ex) { _session?.DebugError("Debugger error: " + Describe(ex)); }
        }
        _stopped = false;
        Program.WriteRaw("D r");
    }

    /// <summary>
    /// Report where execution is held, and why.
    ///
    /// The reason matters to the front end because a step and a breakpoint stop look alike
    /// from the outside: both are a DebuggerStop, and the difference is only whether the
    /// debugger was actually holding a breakpoint at that place. The event says so, and the
    /// window uses it to keep saying "paused" rather than "breakpoint hit" after a Step Over.
    /// </summary>
    private void ReportStopped(DebuggerStopEventArgs e)
    {
        InvocationInfo invocation = e.InvocationInfo;
        string script = invocation?.ScriptName ?? "";
        int line = invocation?.ScriptLineNumber ?? 0;
        int col = Math.Max(0, (invocation?.OffsetInLine ?? 0) + 1);
        bool onBreakpoint = e.Breakpoints != null && e.Breakpoints.Count > 0;
        Program.WriteRaw($"D s {(onBreakpoint ? "breakpoint" : "step")} {line} {col} {Program.Encode(script)}");
    }

    /// <summary>Queue a command while stopped.</summary>
    public bool TryQueue(string verb, string argument)
    {
        if (!_stopped) return false;
        try { _requests.Add(new Request { Verb = verb, Argument = argument }); return true; }
        catch (InvalidOperationException) { return false; }
    }

    /// <summary>Handle one request while stopped.</summary>
    private void Handle(Request req)
    {
        switch (req.Verb)
        {
            // Breakpoints, whether asked for by name or by an edit: the answer is the list
            // either way, because the front end's gutter is drawn from the list and an edit it
            // has to guess the result of is a gutter that disagrees with the engine.
            case "b":
            case "l":
                WriteBreakpoints(req.Verb == "l" ? string.Empty : req.Argument);
                break;
            // The answer's kind is "v", like the verb. It was "l" for a while, which read as
            // the list of breakpoints on the way in and as the variables on the way out, and
            // the front end cannot guess which of the two a half-received answer is.
            case "v":
                if (int.TryParse(req.Argument, out int depth) && depth >= 0 && depth <= 20)
                {
                    Program.WriteChunked("D", "v", Inspect(VariablesScript(depth)), NextReply());
                }
                else
                {
                    _session?.DebugError("That is not a stack depth: " + req.Argument);
                }
                break;
            case "k": Program.WriteChunked("D", "k", CallStackRows(), NextReply()); break;
            case "p": Program.WriteChunked("D", "p", Evaluate(req.Argument), NextReply()); break;
            default: _session?.DebugError("Unknown debugger command '" + req.Verb + "'."); break;
        }
    }

    /// <summary>Queue a resume verb.</summary>
    public void RequestResume(string verb)
    {
        if (!ResumeVerb(verb).HasValue) return;
        try { _requests.Add(new Request { Verb = verb }); }
        catch (InvalidOperationException) { }
    }

    /// <summary>Resume verb lookup.</summary>
    internal static DebuggerResumeAction? ResumeVerb(string verb) => verb switch
    {
        "c" => DebuggerResumeAction.Continue,
        "i" => DebuggerResumeAction.StepInto,
        "o" => DebuggerResumeAction.StepOver,
        "u" => DebuggerResumeAction.StepOut,
        "q" => DebuggerResumeAction.Stop,
        _ => null,
    };

    // ------------------------------------------------------------------ breakpoints

    /// <summary>
    /// Answers a breakpoint request on the calling thread.
    ///
    /// Breakpoints are the one debugger thing that is not about a stop: the user sets one
    /// while the script is sitting at the prompt, and expects it to be there when the script
    /// reaches that line. Going through the stop loop would make F9 work only while already
    /// paused, which is not a debugger anyone would recognise.
    ///
    /// The debugger object serialises this against its own work, and every call here takes
    /// _dbgGate, so a breakpoint set while the shell is running cannot land in the middle of a
    /// stopped frame's own list. The frame that gets inspected is a different matter, and that
    /// is why the verbs left to the stop loop are the frame's.
    /// </summary>
    /// <param name="verb">"b" to edit, "l" to list.</param>
    /// <param name="argument">The edit, empty for a plain listing.</param>
    /// <returns>True when this was a breakpoint request and has been answered.</returns>
    public bool TryAnswerBreakpoints(string verb, string argument)
    {
        if (verb != "b" && verb != "l") return false;
        try
        {
            WriteBreakpoints(verb == "l" ? string.Empty : argument);
        }
        catch (Exception ex) { Notice(Describe(ex)); }
        return true;
    }

    /// <summary>Writes the breakpoint list, having applied the edit, if there was one.</summary>
    private void WriteBreakpoints(string spec) => Program.WriteChunked(
        "D", "b",
        string.IsNullOrEmpty(spec) ? BreakpointRows() : EditBreakpoint(spec),
        NextReply());

    /// <summary>Set/remove/toggle/clear breakpoints; answer with the list.</summary>
    public string EditBreakpoint(string spec)
    {
        Debugger dbg = _dbg;
        if (dbg == null) return Rows(RowOf("e", "No runspace."));

        string verb = spec.Length > 0 ? spec.Substring(0, 1) : "";
        // What follows the verb is its operand, written against it: "+12 C:\dir\script.ps1"
        // asks for line 12. A space between the two is accepted too, because the difference is
        // invisible in a record the user never sees, and a breakpoint that silently fails to be
        // placed is a nasty thing to diagnose.
        string rest = spec.Length > 1 ? spec.Substring(1).TrimStart() : "";

        try
        {
            switch (verb)
            {
                case "+": if (rest.Length > 0) SetOne(dbg, rest); break;
                case "-": if (int.TryParse(rest, out int id)) { lock (_dbgGate) { var bp = dbg.GetBreakpoint(id); if (bp != null) dbg.RemoveBreakpoint(bp); } } break;
                case "e": if (int.TryParse(rest, out int tid)) { lock (_dbgGate) { var bp = dbg.GetBreakpoint(tid); if (bp != null) dbg.EnableBreakpoint(bp); } } break;
                case "d": if (int.TryParse(rest, out int tid2)) { lock (_dbgGate) { var bp = dbg.GetBreakpoint(tid2); if (bp != null) dbg.DisableBreakpoint(bp); } } break;
                case "c": lock (_dbgGate) { foreach (var bp in dbg.GetBreakpoints().ToList()) dbg.RemoveBreakpoint(bp); } break;
                default: return Rows(RowOf("e", "Unknown breakpoint command '" + verb + "'."));
            }
        }
        catch (Exception ex) { return Rows(RowOf("e", Describe(ex))); }

        UpdateMode();
        return BreakpointRows();
    }

    /// <summary>Create a line breakpoint.</summary>
    private void SetOne(Debugger dbg, string rest)
    {
        int space = rest.IndexOf(' ');
        if (space <= 0) throw new ArgumentException("Need line and path.");
        if (!int.TryParse(rest.Substring(0, space), out int line) || line < 1) throw new ArgumentException("Bad line: " + rest.Substring(0, space));
        string path = rest.Substring(space + 1).Trim();
        if (path.Length == 0) throw new ArgumentException("Need path.");
        string full = Path.GetFullPath(path);
        if (!File.Exists(full)) throw new FileNotFoundException("Save first: " + full);
        // The same gate every other debugger call takes. Two locks - this object and
        // _dbgGate - would be one too many to keep straight, and a breakpoint set while the
        // front end is listing them is exactly as surprising as it sounds.
        lock (_dbgGate) { dbg.SetLineBreakpoint(full, line, 0, null); }
    }

    /// <summary>List breakpoints.</summary>
    public string BreakpointRows()
    {
        Debugger dbg = _dbg;
        if (dbg == null) return Rows(RowOf("e", "Cannot read the breakpoints: no runspace."));

        List<Breakpoint> points;
        try { lock (_dbgGate) { points = dbg.GetBreakpoints().ToList(); } }
        catch (Exception ex) { return Rows(RowOf("e", Describe(ex))); }

        var rows = new List<string>(points.Count);
        foreach (Breakpoint bp in points)
        {
            int line = bp is LineBreakpoint lb ? lb.Line : 0;
            rows.Add(RowOf(bp.Id.ToString(), bp.Script ?? "", line.ToString(), bp.Enabled ? "1" : "0", bp.HitCount.ToString()));
        }
        return Rows(rows.ToArray());
    }

    // ------------------------------------------------------------------ inspection

    /// <summary>
    /// Answers an expression asked for in the Immediate box: what it is worth, or the reason it
    /// is worth nothing.
    ///
    /// The expression is parsed before it is run, because PowerShell reports a syntax error
    /// against the script it was handed, and the script it is handed here is the one this window
    /// wraps the expression in. Measured: <c>$nothing-here</c> answered with "At line:10 char:36
    /// + foreach ($item in (& { $nothing-here } 2>&1))" - a complaint about the window's own code,
    /// at a line number nobody can look up, printed above the sentence that was actually about
    /// what was typed. The parser answers with that sentence on its own.
    /// </summary>
    private string Evaluate(string expression)
    {
        if (!string.IsNullOrWhiteSpace(expression))
        {
            System.Management.Automation.Language.Token[] tokens;
            System.Management.Automation.Language.ParseError[] errors;
            try
            {
                System.Management.Automation.Language.Parser.ParseInput(expression, out tokens, out errors);
            }
            catch (Exception ex) { return Rows(RowOf("e", Describe(ex))); }

            if (errors != null && errors.Length > 0)
            {
                var rows = errors.Take(3).Select(err => RowOf("e", Clean(err.Message))).ToList();
                if (errors.Length > rows.Count)
                {
                    rows.Add(RowOf("e", Clean("... and " + (errors.Length - rows.Count) + " more.")));
                }
                return Rows(rows.ToArray());
            }
        }

        return Inspect(EvaluateScript(expression));
    }

    /// <summary>Runs a script in the stopped frame and turns what came back into payload rows.</summary>
    private string Inspect(string script)
    {
        Debugger dbg = _dbg;
        if (dbg == null) return Rows(RowOf("e", "Cannot read the session: no runspace."));

        var command = new PSCommand();
        command.AddScript(script);

        var output = new PSDataCollection<PSObject> { EnumeratorNeverBlocks = true };

        try
        {
            lock (_dbgGate) { dbg.ProcessCommand(command, output); }
        }
        catch (Exception ex)
        {
            return Rows(RowOf("e", Describe(ex)));
        }

        IList<PSObject> results;
        try { results = output.ReadAll(); }
        catch (Exception ex) { return Rows(RowOf("e", Describe(ex))); }

        var rows = new List<string>(results.Count);
        foreach (PSObject item in results)
        {
            // A failure PowerShell reported rather than threw arrives here as ordinary output
            // carrying an error record - "that scope does not exist" is one - and a record's
            // ToString is its message. Taken for a value it would appear in the Variables pane
            // as a variable whose name is a sentence, so it is marked as what it is.
            if (item?.BaseObject is ErrorRecord failure)
            {
                rows.Add(RowOf("e", Clean(failure.Exception?.Message ?? failure.ToString())));
                continue;
            }

            string text = item?.ToString() ?? string.Empty;
            if (text.Length == 0) continue;
            // A row the script already marked is its own kind of answer - an error, or the
            // deliberate "this frame has nothing in it" - and must reach the front end as it
            // was written. Anything else is a value the script printed plainly, which becomes
            // one.
            rows.Add(text.StartsWith("v" + Field, StringComparison.Ordinal) ||
                     text.StartsWith("e" + Field, StringComparison.Ordinal) ||
                     text.StartsWith("n" + Field, StringComparison.Ordinal)
                ? text
                : RowOf("v", Clean(text)));
        }

        // Silence from a command that was asked to print something is a failure worth naming:
        // a stopped-on frame that really has no variables says so with its own row, so getting
        // nothing back here means the read did not happen.
        return rows.Count > 0 ? Rows(rows.ToArray()) : Rows(RowOf("e", "The expression returned nothing."));
    }

    /// <summary>
    /// Lists the variables one stack frame introduces.
    ///
    /// The depth is the frame, in PowerShell's own counting: 0 is the frame execution is held
    /// in, 1 its caller, and so on up to the script that started it. That is what makes the Call
    /// Stack pane work as a scope chooser - clicking a frame asks for that depth - and it needs
    /// no translation because <c>Get-Variable -Scope</c> counts the same way. Two measured facts
    /// shape the script below.
    ///
    /// <c>Get-Variable -Scope n</c> answers with the variables <em>of</em> that scope, not with
    /// everything it can see: stopped inside a five line function, the answer for its own scope
    /// was two rows, while the same frame reading the same session from the prompt is a scope
    /// holding the session's few hundred. A script dot-sourced at the prompt therefore lists the
    /// session when it stops at its own top level, which is right - there the script <em>is</em>
    /// that scope, and its variables are the ones the user is working with. It is shown from the
    /// first row for the same reason.
    ///
    /// A scope that is not there says so with a binding error that survives
    /// <c>-ErrorAction SilentlyContinue</c>, so every scope read is inside a <c>try</c>. A frame
    /// beyond the outermost script - reachable by clicking the last row of the Call Stack pane,
    /// because frames and scopes part company on a dot-sourced script, which is one scope and two
    /// frames - is that error, and it is passed up as one error row rather than hidden.
    ///
    /// The read runs inside a script block, which is the whole of its hygiene. A block is a scope
    /// of its own, so everything it needs - <c>$us</c>, <c>Clean</c>, its loop variables, the
    /// <c>$foreach</c> enumerator a loop creates - belongs to the block and dies with it, instead
    /// of being left behind in the frame the user is looking at. Leaving them there is what put
    /// <c>$mine</c>, <c>$before</c> and a <c>foreach</c> row into the Variables pane in the first
    /// measurements; no amount of <c>Remove-Variable</c> afterwards is as good as never putting
    /// them there. Being one scope deeper is also why every scope number is <c>DEPTH + 1</c>.
    ///
    /// A frame with nothing in it reports one note, and a frame that cannot be read reports one
    /// error, because nothing at all is the same answer as a failure and neither of them is the
    /// same answer as an empty scope.
    /// </summary>
    private static string VariablesScript(int depth)
    {
        // Substituted on the whole script rather than on the fragment a placeholder happens to
        // sit in: `.Replace` applies to the string it is called on, and DEPTH appears in more than
        // one of the fragments below. That is how `-Scope DEPTH` once reached PowerShell as a bare
        // word and turned every variable list into a parameter binding error.
        string script = string.Concat(
            @"& {
$us = [string][char]", (int)Field, @";
function Clean($text) {
    if ($null -eq $text) { return '' }
    $s = [string]$text
    $s = $s -replace '[\x00-\x1f]', ' '
    if ($s.Length -gt 240) { $s = $s.Substring(0, 240) + '...' }
    return $s
}
$mine = @()
try { $mine = @(Get-Variable -Scope (DEPTH + 1) -ErrorAction Stop) }
catch { 'e' + $us + (Clean $_.Exception.Message); return }
$rows = foreach ($v in ($mine | Sort-Object Name)) {
    # PowerShell's own variables, by the flag it puts on them. A scope read from outside the frame
    # - which is how this script has to read it - answers a function's two parameters with $? and
    # $false and $Host and $PSVersionTable beside them, because PowerShell hands those to every
    # scope it makes, and a pane that leads with them hides what the user came to read. The flag
    # is AllScope, not Automatic: measured, Automatic is not a member of ScopedItemOptions in
    # 7.6, so a test against it silently filters nothing, which is worse than not testing.
    $machinery = $false
    try { $machinery = [bool]($v.Options -band [System.Management.Automation.ScopedItemOptions]::AllScope) } catch { }
    if ($machinery) { continue }
    # And the ones that come with no flag at all: what a command was called with, what a pipeline
    # is carrying, where the script is running from, and $PSDebugContext, which is in a frame only
    # because execution is held and so belongs to the debugger rather than to the script.
    # And the ones that come with no flag at all. Two kinds, and the difference matters: $? and
    # $null and $foreach are PowerShell's own bookkeeping, so they go; $Matches and $this are set
    # *by the script* and mean something the reader wants, so they stay even though they are
    # automatic. $PSDebugContext is in a frame only because execution is held, which makes it a
    # mark of the debugger rather than of the script.
    if ('args','input','PSBoundParameters','MyInvocation','PSScriptRoot','PSCommandPath','PSItem','_','Error','PSDebugContext','foreach','stack','switch','null','true','false','?','^','$','$$' -contains $v.Name) { continue }
    $value = $null
    $type = '<null>'
    try {
        $value = $v.Value
        if ($null -ne $value) { $type = $value.GetType().Name }
    } catch { $type = '<unreadable>' }
    'v' + $us + $v.Name + $us + $type + $us + (Clean $value)
}
if (@($rows).Count -eq 0) { 'n' + $us + 'This frame has no variables of its own.' } else { $rows }
}
");
        return script.Replace("DEPTH", depth.ToString(CultureInfo.InvariantCulture));
    }

    /// <summary>
    /// Evaluates an expression the user typed, in the stopped frame.
    ///
    /// Two things the measurement forced into this script. An unknown command does not raise:
    /// it arrives on the success stream holding the error text, so the streams are merged and
    /// an ErrorRecord is marked as one, which is the only way the pane can tell a value
    /// from a complaint. And the body runs inside a script block, so a stray return or a
    /// break in what the user typed ends the block rather than the stop loop.
    /// </summary>
    private static string EvaluateScript(string expression)
    {
        string script = string.Concat(
        "$us = [string][char]", (int)Field, @";
function Clean($text) {
    if ($null -eq $text) { return '<null>' }
    $s = [string]$text
    $s = $s -replace '[\x00-\x1f]', ' '
    if ($s.Length -gt 240) { $s = $s.Substring(0, 240) + '...' }
    return $s
}
try {
    foreach ($item in (& { BODY } 2>&1)) {
        if ($item -is [System.Management.Automation.ErrorRecord]) {
            'e' + $us + (Clean $item.Exception.Message)
            continue
        }
        $type = '<null>'
        if ($null -ne $item) { try { $type = $item.GetType().Name } catch { $type = '<unknown>' } }
        'v' + $us + $type + $us + (Clean $item)
    }
} catch {
    'e' + $us + (Clean $_.Exception.Message)
}
");
        // Substituted on the whole script, as in VariablesScript, so the placeholder is replaced
        // whichever fragment it ends up in.
        return script.Replace("BODY", string.IsNullOrWhiteSpace(expression) ? "$null" : expression);
    }

    // ------------------------------------------------------------------ call stack

    /// <summary>
    /// The frames execution is held between, innermost first.
    ///
    /// The first field is the frame's place in the call stack, which is also what a variables
    /// request asks for: <c>Get-Variable -Scope 0</c> reads the frame execution is held in,
    /// <c>-Scope 1</c> its caller, and so on, in the same order as here. The two agree because
    /// both count script scopes from the inside out. They can drift apart on a stack with
    /// frames that hold no scope of their own, which PowerShell's own <c>Get-PSCallStack</c>
    /// has the same trouble with; the frame's own function name is shown beside it so the user
    /// can see which frame they are actually reading.
    /// </summary>
    private string CallStackRows()
    {
        Debugger dbg = _dbg;
        if (dbg == null) return Rows(RowOf("e", "Cannot read the call stack: no runspace."));

        List<CallStackFrame> frames;
        try { lock (_dbgGate) { frames = dbg.GetCallStack().ToList(); } }
        catch (Exception ex) { return Rows(RowOf("e", Describe(ex))); }

        var rows = new List<string>(frames.Count);
        for (int i = 0; i < frames.Count; i++)
        {
            var f = frames[i];
            rows.Add(RowOf(i.ToString(), string.IsNullOrWhiteSpace(f.FunctionName) ? "<script>" : f.FunctionName, Clean(f.ScriptName ?? ""), f.ScriptLineNumber.ToString()));
        }
        return rows.Count > 0
            ? Rows(rows.ToArray())
            : Rows(RowOf("e", "The call stack is empty: the shell is not stopped in a script."));
    }

    // ------------------------------------------------------------------ helpers

    private static string Rows(params string[] rows) => string.Join(Row.ToString(), rows ?? Array.Empty<string>());

    private static string RowOf(params string[] fields) => string.Join(Field.ToString(), (fields ?? Array.Empty<string>()).Select(Clean));

    private static string Clean(string text)
    {
        if (string.IsNullOrEmpty(text)) return "";
        if (text.Any(c => c < ' ')) { var sb = new StringBuilder(text.Length); foreach (char c in text) sb.Append(c < ' ' ? ' ' : c); return sb.ToString(); }
        return text;
    }

    private static string Describe(Exception ex) => string.IsNullOrWhiteSpace(ex?.Message) ? "Debugger error: " + ex?.GetType().Name + "." : ex.Message;
}
