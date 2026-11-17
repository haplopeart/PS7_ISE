using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Management.Automation;
using System.Management.Automation.Runspaces;
using System.Security;
using System.Text;

namespace PowerShellEngineHost;

/// <summary>
/// Everything the front-end's Commands panel asks about: the list of commands the user can
/// actually run, and the parameters of one of them.
///
/// Two calls, because the two have very different costs in this host. The index is one
/// <c>Get-Command</c> over the session - measured at about a second for the four to five
/// thousand cmdlets and functions a loaded Windows offers, and it does <em>not</em> pull any
/// module into the session, so scanning changes nothing about what the user then sees at the
/// prompt. The price of not importing is that a command from a module nobody has loaded is a
/// stub: it has a name, and sometimes a handful of parameter names, but no attributes. Per
/// command detail is the other call, and it is cheap per command (12-14 ms) precisely
/// because it is one command; asking for it is also what imports the module, which is what
/// the original ISE did too, since that is how its help pane was filled.
///
/// The fields come from <see cref="ParameterAttribute"/> and from the parameter's
/// <see cref="ParameterMetadata.ParameterType"/>, deliberately not from the convenient
/// properties on <see cref="ParameterMetadata"/>: measured in this very host,
/// <c>IsMandatory</c> and <c>Position</c> come back as nothing at all, and
/// <c>SwitchParameter</c> answers true for every parameter including a plain
/// <c>string[]</c> one. The attribute carries the same information correctly, and a switch is
/// simply a parameter whose type is <see cref="SwitchParameter"/>.
/// </summary>
public sealed class CommandMetadata
{
    /// <summary>Separates one record from the next inside a reply.</summary>
    internal const char Row = '\u001e';

    /// <summary>Separates the fields of a command record.</summary>
    internal const char Field = '\u001f';

    /// <summary>Separates the fields of a parameter record, so the two row kinds cannot be confused.</summary>
    internal const char Part = '\u001d';

    /// <summary>The command's own help text is long; this is plenty for a pane and keeps one
    /// command's reply to a single chunk.</summary>
    private const int MaxText = 4000;

    /// <summary>
    /// The parameters PowerShell adds to every cmdlet. They are in a command's own parameter
    /// dictionary - <c>-WhatIf</c> only when the cmdlet supports ShouldProcess - so the panel
    /// is told which ones these are rather than guessing from a description.
    /// </summary>
    private static readonly HashSet<string> CommonParameters = new(StringComparer.OrdinalIgnoreCase)
    {
        "Verbose", "Debug", "ErrorAction", "ErrorVariable", "WarningAction", "WarningVariable",
        "InformationAction", "InformationVariable", "OutVariable", "OutBuffer", "PipelineVariable",
        "ProgressAction", "WhatIf", "Confirm",
    };

    private readonly object _gate = new();
    private string _index;

    /// <summary>The read in progress, if there is one, so it can be given up.</summary>
    private volatile PowerShell _inFlight;

    /// <summary>
    /// Gives up a read in progress. Stopping a pipeline from another thread is the one thing
    /// that is safe to do to it. The front end cannot ask for this while a list is being read -
    /// the thread writing it is not reading commands - so what it is for is shutting down, where
    /// the runspace is about to be disposed and a read that will not finish must not stand in the
    /// way. A read that was given up is not cached: a list with half of it missing is worse than
    /// no list, so the caller is told nothing was answered.
    /// </summary>
    public void Cancel()
    {
        try
        {
            _inFlight?.Stop();
        }
        catch
        {
            // A pipeline that will not stop is on its way out anyway.
        }
    }

    /// <summary>Drops the scanned list so the next request walks the session again.</summary>
    public void Invalidate()
    {
        lock (_gate)
        {
            _index = null;
        }
    }

    /// <summary>
    /// One row per command: name, module, and the parameter names PowerShell will already
    /// tell us without importing anything. Cached, because the scan is the expensive thing
    /// the panel must not pay for twice. Given back as null when the read was given up on, which
    /// means "ask when it is quiet"; a read that came back with nothing in it is thrown instead,
    /// because that is not a thing to wait for.
    /// </summary>
    public string BuildIndex(Runspace runspace)
    {
        lock (_gate)
        {
            if (_index != null) return _index;
        }

        var sb = new StringBuilder(512 * 1024);
        try
        {
            using (PowerShell ps = PowerShell.Create())
            {
                _inFlight = ps;
                ps.Runspace = runspace;
                ps.AddCommand("Get-Command")
                  .AddParameter("CommandType", new CommandTypes[] { CommandTypes.Cmdlet, CommandTypes.Function });

                foreach (CommandInfo info in ps.Invoke<CommandInfo>())
                {
                    string name = Clean(info.Name);
                    if (name.Length == 0) continue;

                    string module = Clean(info.ModuleName ?? string.Empty);
                    IEnumerable<string> names = Array.Empty<string>();
                    try
                    {
                        if (info.Parameters != null) names = info.Parameters.Keys;
                    }
                    catch
                    {
                        // A stub with nothing to say: the detail call is the one that can answer.
                    }

                    sb.Append(name).Append(Field).Append(module).Append(Field);
                    sb.Append(string.Join(",", names.Select(Clean)));
                    sb.Append(Row);
                }
            }
        }
        catch (PipelineStoppedException)
        {
            // Someone submitted a command while this was reading. Nothing is cached and
            // nothing is said to be known: the caller says the session is busy, and the panel
            // asks again once it is not.
            return null;
        }
        finally
        {
            _inFlight = null;
        }

        string built = sb.ToString();
        if (built.Length == 0)
        {
            // Get-Command came back with nothing in it. That is not a session with no commands,
            // it is the scan having gone wrong, and remembering it would leave the panel empty
            // for the rest of the session with nothing the user could do about it. So nothing is
            // cached, and this is said as a failure rather than as a busy answer: the console is
            // not running anything, the list simply could not be read, and the panel should say
            // that rather than tell the user to wait for a command that has already finished.
            throw new InvalidOperationException("The engine read the session and found no commands to list.");
        }

        lock (_gate)
        {
            _index = built;
        }
        return built;
    }

    /// <summary>
    /// One command, as much about it as the panel shows: the syntax block, the synopsis, and
    /// every parameter with the type, mandatory flag, position, switch flag and whether it is
    /// a common parameter. Throws when the name is not a command, which the caller turns into
    /// an error record rather than an empty reply, so "no such command" and "nothing to show"
    /// stay distinguishable. Given back as null when the read was given up because the user
    /// submitted a command, which is the same answer as the session being busy.
    /// </summary>
    public string BuildDetail(Runspace runspace, string name)
    {
        try
        {
            return Describe(runspace, name);
        }
        catch (PipelineStoppedException)
        {
            return null;
        }
    }

    private string Describe(Runspace runspace, string name)
    {
        CommandInfo info = Lookup(runspace, name);
        if (info == null) throw new InvalidOperationException("No command found with the name '" + name + "'.");

        // The list does not contain aliases (see BuildIndex), but a name the user typed might
        // be one, and an alias has no parameters of its own: `cd` is not the command, so the
        // panel has to describe the command behind it. An alias' own Definition is the name
        // it stands for.
        string shown = info.Name;
        if (info is AliasInfo alias)
        {
            CommandInfo target = Lookup(runspace, alias.Definition ?? string.Empty);
            if (target != null) info = target;
        }

        string synopsis = string.Empty;
        try
        {
            // Help is worth the 33 ms it costs, and it is what imports the module. A command
            // with no help content simply has nothing to report, which is not an error.
            using PowerShell ps = PowerShell.Create();
            _inFlight = ps;
            try
            {
                ps.Runspace = runspace;
                ps.AddCommand("Get-Help").AddParameter("Name", info.Name);
                foreach (PSObject item in ps.Invoke())
                {
                    PSPropertyInfo property = item?.Properties["Synopsis"];
                    if (property?.Value != null) synopsis = Clean(property.Value.ToString());
                    break;
                }
            }
            finally
            {
                _inFlight = null;
            }
        }
        catch (PipelineStoppedException)
        {
            // The user submitted a command while help was being read. That is not a command
            // with nothing to report, it is an answer that was stopped, so it goes up.
            throw;
        }
        catch
        {
            // Leave the synopsis empty; the syntax line below still describes the command.
        }

        string syntax = Clean(info.Definition ?? string.Empty);

        // With no help file installed the synopsis PowerShell synthesises is the syntax line
        // itself, and repeating four hundred characters twice in every reply would be silly.
        if (string.Equals(synopsis, syntax, StringComparison.Ordinal)) synopsis = string.Empty;
        if (syntax.Length > MaxText) syntax = syntax.Substring(0, MaxText);
        if (synopsis.Length > MaxText) synopsis = synopsis.Substring(0, MaxText);

        var sb = new StringBuilder(1024);
        sb.Append('C').Append(Field).Append(Clean(shown)).Append(Field).Append(Clean(info.ModuleName ?? string.Empty))
          .Append(Field).Append(synopsis).Append(Field).Append(syntax).Append(Row);

        foreach (KeyValuePair<string, ParameterMetadata> pair in Parameters(info))
        {
            ParameterMetadata metadata = pair.Value;
            Type type = metadata?.ParameterType;

            // A parameter can appear in several parameter sets and carries one attribute per
            // set. It is mandatory if any set demands it, and positional at the lowest place
            // any set gives it - which is what the syntax line shows, and the two readings
            // were checked against each other.
            bool mandatory = false;
            int position = -1;
            foreach (Attribute attribute in metadata?.Attributes ?? Enumerable.Empty<Attribute>())
            {
                if (attribute is not ParameterAttribute parameter) continue;
                if (parameter.Mandatory) mandatory = true;
                if (parameter.Position >= 0 && (position < 0 || parameter.Position < position)) position = parameter.Position;
            }

            bool isSwitch = type == typeof(SwitchParameter);

            sb.Append('P').Append(Part).Append(Clean(pair.Key)).Append(Part).Append(TypeName(type))
              .Append(Part).Append(mandatory ? "1" : "0").Append(Part)
              .Append(position.ToString(CultureInfo.InvariantCulture)).Append(Part)
              .Append(isSwitch ? "1" : "0").Append(Part)
              .Append(CommonParameters.Contains(pair.Key) ? "1" : "0")
              // The description slot is kept so the row always has the same number of fields:
              // Get-Help -Parameter returns nothing at all for a command with no help file,
              // which is most of them on a machine without help installed.
              .Append(Part).Append(string.Empty).Append(Row);
        }

        return sb.ToString();
    }

    private CommandInfo Lookup(Runspace runspace, string name)
    {
        if (string.IsNullOrWhiteSpace(name)) return null;

        using PowerShell ps = PowerShell.Create();
        _inFlight = ps;
        try
        {
            ps.Runspace = runspace;
            ps.AddCommand("Get-Command").AddParameter("Name", name);
            foreach (CommandInfo info in ps.Invoke<CommandInfo>()) return info;
            return null;
        }
        finally
        {
            _inFlight = null;
        }
    }

    /// <summary>
    /// A command's parameters, or none. Functions that are not proxy functions, executables
    /// and scripts on the list have no parameter metadata to give, and asking never hurts
    /// them.
    /// </summary>
    private static IEnumerable<KeyValuePair<string, ParameterMetadata>> Parameters(CommandInfo info)
    {
        try
        {
            return info?.Parameters ?? Enumerable.Empty<KeyValuePair<string, ParameterMetadata>>();
        }
        catch
        {
            return Enumerable.Empty<KeyValuePair<string, ParameterMetadata>>();
        }
    }

    /// <summary>
    /// The type the way PowerShell writes it in its own syntax line: <c>string[]</c>, not
    /// <c>String[]</c>. Anything outside the short list of names PowerShell shortens is shown
    /// as its own class name.
    /// </summary>
    private static string TypeName(Type type)
    {
        if (type == null) return "object";
        if (type.IsArray) return TypeName(type.GetElementType()) + "[]";

        if (type == typeof(SwitchParameter)) return "switch";
        if (type == typeof(string)) return "string";
        if (type == typeof(int)) return "int";
        if (type == typeof(long)) return "long";
        if (type == typeof(double)) return "double";
        if (type == typeof(decimal)) return "decimal";
        if (type == typeof(bool)) return "bool";
        if (type == typeof(byte)) return "byte";
        if (type == typeof(char)) return "char";
        if (type == typeof(DateTime)) return "datetime";
        if (type == typeof(TimeSpan)) return "timespan";
        if (type == typeof(Guid)) return "guid";
        if (type == typeof(Uri)) return "uri";
        if (type == typeof(Hashtable)) return "hashtable";
        if (type == typeof(SecureString)) return "securestring";
        if (type == typeof(object)) return "object";
        if (typeof(PSObject).IsAssignableFrom(type)) return "psobject";

        return Clean(type.Name);
    }

    /// <summary>
    /// Free text from a command - a module name, a definition, a help string - made safe to
    /// sit between the separators. Carriage returns go because the front-end draws the syntax
    /// block itself; the record separators go because they would split a row in half.
    /// </summary>
    private static string Clean(string text)
    {
        if (string.IsNullOrEmpty(text)) return string.Empty;

        var sb = new StringBuilder(text.Length);
        foreach (char c in text)
        {
            if (c == '\r') continue;
            if (c == Row || c == Field || c == Part) continue;
            sb.Append(c);
        }
        return sb.ToString();
    }
}
