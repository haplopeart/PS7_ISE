using System;
using System.Collections.Concurrent;
using System.Management.Automation;
using System.Management.Automation.Host;
using System.Text;

namespace PowerShellEngineHost;

/// <summary>Kind of an output event, used by the front-end to pick a color/behaviour.</summary>
public static class EventKinds
{
    public const int Output = 1;
    public const int Error = 2;
    public const int Warning = 3;
    public const int Verbose = 4;
    public const int Debug = 5;
    public const int Progress = 6;
    public const int Title = 7;
    public const int CommandStarted = 8;
    public const int CommandFinished = 9;
    public const int InputRequested = 10;
    public const int InputEcho = 11;
    /// <summary>
    /// One module's verdict from the background verifier: whether it imports into a fresh
    /// PowerShell 7 session and, if not, why. The Commands panel uses these to hide commands
    /// whose module will not load, and to say why.
    /// </summary>
    public const int Verification = 12;
}

/// <summary>One colored run of text destined for the console pane.</summary>
public sealed class OutputEvent
{
    public int Kind;
    public ConsoleColor Fg;
    public ConsoleColor Bg;
    public string Text;

    public OutputEvent(int kind, ConsoleColor fg, ConsoleColor bg, string text)
    {
        Kind = kind;
        Fg = fg;
        Bg = bg;
        Text = text ?? string.Empty;
    }
}

/// <summary>
/// The host that PowerShell runs inside of. This is the piece that makes the console
/// "real": every string PowerShell wants to display arrives here, and every line of
/// input the shell needs is taken from here.
/// </summary>
public sealed class ISEHost : PSHost
{
    private readonly Guid _instanceId = Guid.NewGuid();
    private readonly ISEHostUserInterface _ui;

    public ISEHost(Session session)
    {
        Session = session;
        _ui = new ISEHostUserInterface(session);
    }

    public Session Session { get; }

    public override Guid InstanceId => _instanceId;

    public override string Name => "PS7 ISE Host";

    // Must look like PowerShell 7 so that $PSVersionTable.HostName/Version behave.
    public override Version Version => new Version(7, 4, 0);

    public override PSHostUserInterface UI => _ui;

    /// <summary>
    /// The colour plumbing, typed. The default pair and <c>ResetColor</c> live on the raw
    /// interface, and going through <see cref="UI"/> would mean casting at every use.
    /// </summary>
    public ISERawUserInterface RawInterface => _ui.Raw;

    public override System.Globalization.CultureInfo CurrentCulture
        => System.Globalization.CultureInfo.CurrentCulture;

    public override System.Globalization.CultureInfo CurrentUICulture
        => System.Globalization.CultureInfo.CurrentUICulture;

    public override void EnterNestedPrompt()
        => throw new NotImplementedException("Nested prompts are not supported by the PS7 ISE host.");

    public override void ExitNestedPrompt()
        => throw new NotImplementedException("Nested prompts are not supported by the PS7 ISE host.");

    public override void NotifyBeginApplication() { }

    public override void NotifyEndApplication() { }

    public override void SetShouldExit(int exitCode)
        => Session.RequestExit(exitCode);
}

/// <summary>
/// PowerShell's text output channel. Write/WriteLine calls become colored OutputEvents
/// that the Rust console pane renders.
/// </summary>
public sealed class ISEHostUserInterface : PSHostUserInterface
{
    private readonly Session _session;
    private readonly ISERawUserInterface _raw;

    public ISEHostUserInterface(Session session)
    {
        _session = session;
        _raw = new ISERawUserInterface(session);
    }

    public override PSHostRawUserInterface RawUI => _raw;

    /// <summary>The same object typed, so the session can reach the colour defaults.</summary>
    public ISERawUserInterface Raw => _raw;

    public override Dictionary<string, PSObject> Prompt(string caption, string message,
        System.Collections.ObjectModel.Collection<FieldDescription> descriptions)
    {
        if (!string.IsNullOrEmpty(caption)) Write(ErrorColor, ErrorColor, caption + Environment.NewLine);
        if (!string.IsNullOrEmpty(message)) Write(message + Environment.NewLine);

        var result = new Dictionary<string, PSObject>();
        if (descriptions == null) return result;

        foreach (var fd in descriptions)
        {
            if (!string.IsNullOrEmpty(fd.Label)) Write(fd.Label + ": ");
            string value = ReadLine();
            result[fd.Name] = PSObject.AsPSObject(string.IsNullOrEmpty(value) ? null : (object)value);
        }
        return result;
    }

    public override int PromptForChoice(string caption, string message,
        System.Collections.ObjectModel.Collection<ChoiceDescription> choices, int defaultChoice)
    {
        if (!string.IsNullOrEmpty(caption)) WriteLine(caption);
        if (!string.IsNullOrEmpty(message)) WriteLine(message);

        for (int i = 0; i < choices.Count; i++)
        {
            string label = choices[i].Label;
            if (label.StartsWith("&") && label.Length > 1) label = label.Substring(1);
            WriteLine((i == defaultChoice ? "[*] " : "[ ] ") + label);
        }

        Write("Choice> ");
        string answer = ReadLine();
        if (int.TryParse(answer.Trim(), out int picked) && picked >= 0 && picked < choices.Count)
            return picked;
        return defaultChoice;
    }

    public override PSCredential PromptForCredential(string caption, string message, string userName, string targetName)
        => PromptForCredential(caption, message, userName, targetName, PSCredentialTypes.Default, PSCredentialUIOptions.Default);

    public override PSCredential PromptForCredential(string caption, string message, string userName, string targetName,
        PSCredentialTypes allowedCredentialTypes, PSCredentialUIOptions options)
    {
        if (!string.IsNullOrEmpty(caption)) WriteLine(caption);
        if (!string.IsNullOrEmpty(message)) WriteLine(message);

        Write(string.IsNullOrEmpty(userName) ? "User: " : userName + ": ");
        string user = ReadLine();
        if (string.IsNullOrWhiteSpace(user)) user = userName;

        Write("Password: ");
        var password = ReadLineAsSecureString();
        if (string.IsNullOrWhiteSpace(user)) user = " ";
        return new PSCredential(user, password.Length == 0 ? new System.Security.SecureString() : password);
    }

    public override string ReadLine() => _session.ReadLine();

    public override System.Security.SecureString ReadLineAsSecureString()
    {
        var secure = new System.Security.SecureString();
        foreach (char c in _session.ReadLine()) secure.AppendChar(c);
        return secure;
    }

    public override void Write(string value) => Write(_raw.ForegroundColor, _raw.BackgroundColor, value);

    public override void Write(ConsoleColor foregroundColor, ConsoleColor backgroundColor, string value)
        => _session.Emit(EventKinds.Output, foregroundColor, backgroundColor, value);

    public override void WriteLine()
        => Write(_raw.ForegroundColor, _raw.BackgroundColor, Environment.NewLine);

    public override void WriteLine(string value)
        => Write(value + Environment.NewLine);

    public override void WriteLine(ConsoleColor foregroundColor, ConsoleColor backgroundColor, string value)
        => Write(foregroundColor, backgroundColor, value + Environment.NewLine);

    public override void WriteErrorLine(string value)
        => _session.Emit(EventKinds.Error, ConsoleColor.Red, _raw.BackgroundColor, value + Environment.NewLine);

    public override void WriteDebugLine(string message)
        => _session.Emit(EventKinds.Debug, ConsoleColor.Yellow, _raw.BackgroundColor, "DEBUG: " + message + Environment.NewLine);

    public override void WriteVerboseLine(string message)
        => _session.Emit(EventKinds.Verbose, ConsoleColor.Cyan, _raw.BackgroundColor, "VERBOSE: " + message + Environment.NewLine);

    public override void WriteWarningLine(string message)
        => _session.Emit(EventKinds.Warning, ConsoleColor.Yellow, _raw.BackgroundColor, "WARNING: " + message + Environment.NewLine);

    public override void WriteProgress(long sourceId, ProgressRecord record)
    {
        // Render the bar as a single overwritten text line, like the console does.
        if (record.RecordType == ProgressRecordType.Completed)
        {
            _session.Emit(EventKinds.Progress, ConsoleColor.Gray, _raw.BackgroundColor, "\r" + new string(' ', 120) + "\r");
            return;
        }

        var sb = new StringBuilder();
        sb.Append(record.StatusDescription);
        if (record.PercentComplete >= 0)
        {
            int filled = record.PercentComplete * 20 / 100;
            sb.Append(" [");
            sb.Append('#', filled);
            sb.Append('.', Math.Max(0, 20 - filled));
            sb.Append("] ");
            sb.Append(record.PercentComplete).Append('%');
        }
        _session.Emit(EventKinds.Progress, ConsoleColor.Gray, _raw.BackgroundColor, "\r" + sb.ToString());
    }

    private static ConsoleColor ErrorColor => ConsoleColor.Red;
}

/// <summary>
/// A "buffer" style RawUI. The front-end is a text control rather than a character grid,
/// but formatters (Out-Default, Format-Table) require a coherent size model, so a fixed
/// virtual screen is exposed here.
/// </summary>
public sealed class ISERawUserInterface : PSHostRawUserInterface
{
    private readonly Session _session;
    private Size _windowSize = new Size(120, 40);
    private Size _bufferSize = new Size(120, 3000);
    private Coordinates _cursor = new Coordinates(0, 0);
    private string _title = "PowerShell 7 ISE";

    public ISERawUserInterface(Session session)
    {
        _session = session;
    }

    /// <summary>
    /// The colour the next <c>Write</c> without one of its own will use. Fields rather than the
    /// auto-properties this used to be: a command that sets a colour and never puts it back was
    /// then setting it for everything that came after it, the prompt included, because the base
    /// class has no way back from an auto-property. <see cref="ResetColor"/> is how it gets back,
    /// and it goes to the defaults below rather than to a guess.
    /// </summary>
    private ConsoleColor _foreground = ConsoleColor.Gray;

    private ConsoleColor _background = ConsoleColor.Black;

    /// <summary>
    /// The pair the session treats as its own: what the profile left the colours at, which is
    /// what the prompt is drawn in and what everything returns to once the command that changed
    /// them has finished. PowerShell's console keeps this the same way - the prompt is not part
    /// of the last command's output, so it does not inherit the last command's colours.
    /// </summary>
    private ConsoleColor _defaultForeground = ConsoleColor.Gray;

    private ConsoleColor _defaultBackground = ConsoleColor.Black;

    public override ConsoleColor ForegroundColor
    {
        get => _foreground;
        set => _foreground = value;
    }

    public override ConsoleColor BackgroundColor
    {
        get => _background;
        set => _background = value;
    }

    /// <summary>The colour the prompt is drawn in: the session's own, not the last write's.</summary>
    public ConsoleColor DefaultForegroundColor => _defaultForeground;

    public ConsoleColor DefaultBackgroundColor => _defaultBackground;

    /// <summary>
    /// Adopts the colours now in effect as the session's own. Called once the profile has run, so
    /// a profile that says <c>$Host.UI.RawUI.ForegroundColor = 'Green'</c> has said something about
    /// the prompt and not about one command's output.
    /// </summary>
    public void CaptureDefaults()
    {
        _defaultForeground = _foreground;
        _defaultBackground = _background;
    }

    /// <summary>
    /// Puts the colours back the way the session likes them. Nothing calls this today: a command
    /// that sets <c>$Host.UI.RawUI.ForegroundColor</c> means to set it for what follows, the way
    /// the real console does, so the session does not undo it. It stays because
    /// <c>PSHostRawUserInterface</c> has no <c>ResetColor</c> to override, so a script that wants
    /// its colours back has no other way to ask - the front end hands this to one if that ever
    /// comes up.
    /// </summary>
    public void RestoreDefaults()
    {
        _foreground = _defaultForeground;
        _background = _defaultBackground;
    }

    public override int CursorSize { get; set; } = 25;

    /// <summary>Not overridable on this host type; kept for $host.UI.RawUI callers.</summary>
    public bool CursorVisible { get; set; } = true;

    /// <summary>True while a queued keystroke is waiting for ReadKey.</summary>
    public override bool KeyAvailable => _session.KeyAvailable;

    public override Size WindowSize
    {
        get => _windowSize;
        set { _windowSize = value; _bufferSize = new Size(value.Width, _bufferSize.Height); }
    }

    public override Size BufferSize
    {
        get => _bufferSize;
        set { _bufferSize = value; if (value.Width < _windowSize.Width) _windowSize = new Size(value.Width, _windowSize.Height); }
    }

    public override Coordinates CursorPosition
    {
        get => _cursor;
        set => _cursor = value;
    }

    public override Coordinates WindowPosition { get; set; } = new Coordinates(0, 0);

    public override Size MaxWindowSize => _bufferSize;

    public override Size MaxPhysicalWindowSize => _bufferSize;

    public override string WindowTitle
    {
        get => _title;
        set
        {
            _title = value ?? string.Empty;
            _session.Emit(EventKinds.Title, ConsoleColor.Gray, BackgroundColor, _title);
        }
    }

    public override void FlushInputBuffer() => _session.FlushInput();

    public override KeyInfo ReadKey(ReadKeyOptions options)
        => _session.ReadKey((options & ReadKeyOptions.IncludeKeyDown) != 0);

    public override BufferCell[,] GetBufferContents(Rectangle rectangle)
        => new BufferCell[Math.Max(0, rectangle.Bottom - rectangle.Top + 1), Math.Max(0, rectangle.Right - rectangle.Left + 1)];

    public override void ScrollBufferContents(Rectangle source, Coordinates destination, Rectangle clip, BufferCell fill) { }

    public override void SetBufferContents(Coordinates origin, BufferCell[,] contents) { }

    public override void SetBufferContents(Rectangle rectangle, BufferCell fill) { }
}

/// <summary>ConsoleColor to RGB, using the Campbell palette PowerShell 7 ships with.</summary>
public static class Palette
{
    private static readonly int[] Rgb =
    {
        0x0C0C0C, 0xC50F1F, 0x13A10E, 0xC19C00, 0x0037DA, 0x881798, 0x3A96DD, 0xCCCCCC,
        0x767676, 0xE74856, 0x16C60C, 0xF9F1A5, 0x3B78FF, 0xB4009E, 0x61D6D6, 0xF2F2F2,
    };

    /// <summary>Returns R*65536 + G*256 + B.</summary>
    public static int ToRgb(ConsoleColor color)
    {
        int i = (int)color;
        if (i < 0 || i > 15) i = 7;
        return Rgb[i];
    }
}
