using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Globalization;
using System.Management.Automation;
using System.Management.Automation.Runspaces;
using System.Text;

namespace PowerShellEngineHost;

/// <summary>
/// The debugger the front-end drives: breakpoints, the stop, stepping, and the variable and
/// call-stack reads that only make sense while execution is held.
/// </summary>
public sealed class DebugService
{
    /// <summary>Unique reply number for each debugger protocol request.</summary>
    private int _reply;

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
        ReportStopped(e.InvocationInfo);
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

    /// <summary>Report stop position to the front end.</summary>
    private void ReportStopped(InvocationInfo invocation)
    {
        string script = invocation?.ScriptName ?? "";
        int line = invocation?.ScriptLineNumber ?? 0;
        int col = Math.Max(0, (invocation?.OffsetInLine ?? 0) + 1);
        Program.WriteRaw($"D s breakpoint {line} {col} {Program.Encode(script)}");
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
            case "l": Program.WriteChunked("b", BreakpointRows(), _reply++); break;
            case "k": Program.WriteChunked("k", CallStackRows(), _reply++); break;
            case "b": Program.WriteChunked("b", EditBreakpoint(req.Argument), _reply++); break;
            case "v": 
                if (int.TryParse(req.Argument, out int depth) && depth >= 0 && depth <= 20) 
                { 
                    Program.WriteChunked("l", Inspect(VariablesScript(depth)), _reply++); 
                } 
                else 
                { 
                    _session?.DebugError("That is not a stack depth: " + req.Argument); 
                } 
                break;
            case "p": Program.WriteChunked("p", Inspect(EvaluateScript(req.Argument)), _reply++); break;
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

    /// <summary>Set/remove/toggle/clear breakpoints; answer with the list.</summary>
    public string EditBreakpoint(string spec)
    {
        Debugger dbg = _dbg;
        if (dbg == null) return Rows(RowOf("0", "", "0", "No runspace."));

        string verb = spec.Length > 0 ? spec.Substring(0, 1) : "";
        string rest = spec.Length > 2 ? spec.Substring(2) : "";

        try
        {
            switch (verb)
            {
                case "+": if (rest.Length > 0) SetOne(dbg, rest); break;
                case "-": if (int.TryParse(rest, out int id)) { lock (_dbgGate) { var bp = dbg.GetBreakpoint(id); if (bp != null) dbg.RemoveBreakpoint(bp); } } break;
                case "e": if (int.TryParse(rest, out int tid)) { lock (_dbgGate) { var bp = dbg.GetBreakpoint(tid); if (bp != null) dbg.EnableBreakpoint(bp); } } break;
                case "d": if (int.TryParse(rest, out int tid2)) { lock (_dbgGate) { var bp = dbg.GetBreakpoint(tid2); if (bp != null) dbg.DisableBreakpoint(bp); } } break;
                case "c": lock (_dbgGate) { foreach (var bp in dbg.GetBreakpoints().ToList()) dbg.RemoveBreakpoint(bp); } break;
                default: return Rows(RowOf("0", "", "0", "Unknown: '" + verb + "'.")); break;
            }
        }
        catch (Exception ex) { return Rows(RowOf("0", "", "0", Describe(ex))); }

        UpdateMode();
        return BreakpointRows();
    }

    /// <summary>Create a line breakpoint.</summary>
    private static void SetOne(Debugger dbg, string rest)
    {
        int space = rest.IndexOf(' ');
        if (space <= 0) throw new ArgumentException("Need line and path.");
        if (!int.TryParse(rest.Substring(0, space), out int line) || line < 1) throw new ArgumentException("Bad line: " + rest.Substring(0, space));
        string path = rest.Substring(space + 1).Trim();
        if (path.Length == 0) throw new ArgumentException("Need path.");
        string full = Path.GetFullPath(path);
        if (!File.Exists(full)) throw new FileNotFoundException("Save first: " + full);
        lock (dbg) { dbg.SetLineBreakpoint(full, line, 0, null); }
    }

    /// <summary>List breakpoints.</summary>
    public string BreakpointRows()
    {
        Debugger dbg = _dbg;
        if (dbg == null) return "";

        List<Breakpoint> points;
        try { lock (_dbgGate) { points = dbg.GetBreakpoints().ToList(); } }
        catch (Exception ex) { return Rows(RowOf("0", "", "0", Describe(ex))); }

        var rows = new List<string>(points.Count);
        foreach (Breakpoint bp in points)
        {
            int line = bp is LineBreakpoint lb ? lb.Line : 0;
            rows.Add(RowOf(bp.Id.ToString(), bp.Script ?? "", line.ToString(), bp.Enabled ? "1" : "0", bp.HitCount.ToString()));
        }
        return Rows(rows.ToArray());
    }

    // ------------------------------------------------------------------ inspection

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
            string text = item?.ToString() ?? string.Empty;
            if (text.Length == 0) continue;
            rows.Add(text.StartsWith("v" + Field, StringComparison.Ordinal) ||
                     text.StartsWith("e" + Field, StringComparison.Ordinal)
                ? text
                : RowOf("v", Clean(text)));
        }

        return rows.Count > 0 ? Rows(rows.ToArray()) : Rows(RowOf("e", "The expression returned nothing."));
    }

    /// <summary>
    /// Lists the variables of one stack frame.
    ///
    /// The depth is the frame, in PowerShell's own counting: 0 is the frame execution is held
    /// in, 1 its caller, and so on up to the script that started it. That is what makes the
    /// Call Stack pane work as a scope chooser - clicking a frame asks for that depth - and it
    /// needs no translation because it is the same numbering Get-Variable -Scope uses.
    ///
    /// The name is filtered rather than the scope: Get-Variable answers with everything
    /// the frame can see, which includes every global, so the automatic ones PowerShell puts in
    /// every frame are dropped. What is left is what the script of this window actually made.
    /// </summary>
    private static string VariablesScript(int depth) => string.Concat(
        "$us = [string][char]", (int)Field, @";
function Clean($text) {
    if ($null -eq $text) { return '' }
    $s = [string]$text
    $s = $s -replace '[\x00-\x1f]', ' '
    if ($s.Length -gt 240) { $s = $s.Substring(0, 240) + '...' }
    return $s
}
foreach ($v in (Get-Variable -Scope DEPTH -ErrorAction SilentlyContinue | Sort-Object Name)) {
    if ('args','input','PSBoundParameters','MyInvocation','PSScriptRoot','PSCommandPath','PSItem','_' -contains $v.Name) { continue }
    $value = $null
    $type = '<null>'
    try {
        $value = $v.Value
        if ($null -ne $value) { $type = $value.GetType().Name }
    } catch { $type = '<unreadable>' }
    'v' + $us + $v.Name + $us + $type + $us + (Clean $value)
}
".Replace("DEPTH", depth.ToString(CultureInfo.InvariantCulture)));

    /// <summary>
    /// Evaluates an expression the user typed, in the stopped frame.
    ///
    /// Two things the measurement forced into this script. An unknown command does not raise:
    /// it arrives on the success stream holding the error text, so the streams are merged and
    /// an ErrorRecord is marked as one, which is the only way the pane can tell a value
    /// from a complaint. And the body runs inside a script block, so a stray return or a
    /// break in what the user typed ends the block rather than the stop loop.
    /// </summary>
    private static string EvaluateScript(string expression) => string.Concat(
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
".Replace("BODY", string.IsNullOrWhiteSpace(expression) ? "$null" : expression));

    // ------------------------------------------------------------------ call stack

    /// <summary>Call stack frames.</summary>
    private string CallStackRows()
    {
        Debugger dbg = _dbg;
        if (dbg == null) return "";

        List<CallStackFrame> frames;
        try { lock (_dbgGate) { frames = dbg.GetCallStack().ToList(); } }
        catch (Exception ex) { return Rows(RowOf("e", Describe(ex))); }

        var rows = new List<string>(frames.Count);
        for (int i = 0; i < frames.Count; i++)
        {
            var f = frames[i];
            rows.Add(RowOf(i.ToString(), string.IsNullOrWhiteSpace(f.FunctionName) ? "<script>" : f.FunctionName, Clean(f.ScriptName ?? ""), f.ScriptLineNumber.ToString()));
        }
        return Rows(rows.ToArray());
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