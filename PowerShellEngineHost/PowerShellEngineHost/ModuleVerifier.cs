using System;
using System.Collections.Generic;
using System.Linq;
using System.Management.Automation;
using System.Management.Automation.Runspaces;
using System.Threading;

namespace PowerShellEngineHost;

/// <summary>
/// Verifies modules in a separate runspace so importing them does not pollute the main session.
/// </summary>
public sealed class ModuleVerifier
{
    private readonly Session _session;
    private Thread _worker;
    private volatile bool _stop;

    public ModuleVerifier(Session session) => _session = session;

    public void Start()
    {
        _worker = new Thread(Loop) { IsBackground = true, Name = "PS7-ISE-Verifier" };
        _worker.Start();
    }

    public void Stop()
    {
        _stop = true;
        try { _worker?.Join(TimeSpan.FromSeconds(2)); } catch { }
    }

    private void Loop()
    {
        // Wait for the session to build its index.
        var deadline = DateTime.Now.AddSeconds(30);
        while (DateTime.Now < deadline && !_stop)
        {
            var modules = _session.GetMetadata().GetModuleNames().ToList();
            if (modules.Count > 0)
            {
                Verify(modules);
                return;
            }
            Thread.Sleep(1000);
        }
    }

    private void Verify(List<string> modules)
    {
        // One runspace for all verifications: creating a runspace is expensive and doing it
        // 165 times would add another minute to the scan on top of the imports themselves.
        Runspace runspace = null;
        try
        {
            runspace = RunspaceFactory.CreateRunspace(InitialSessionState.CreateDefault2());
            runspace.Open();

            foreach (string name in modules)
            {
                if (_stop) break;

                string error = null;
                try
                {
                    using var ps = PowerShell.Create();
                    ps.Runspace = runspace;
                    ps.AddCommand("Import-Module").AddParameter("Name", name);
                    ps.Invoke();
                    if (ps.HadErrors && ps.Streams.Error.Count > 0)
                    {
                        error = ps.Streams.Error[0].ToString().Trim();
                    }
                }
                catch (Exception ex)
                {
                    error = ex.Message;
                }

                if (error != null)
                {
                    // A failed import is not always a dead module. Microsoft.PowerShell.Core, for
                    // one, has no module file to import - it is the core that is already in every
                    // session - and asking for it produces "no valid module file" while its sixty
                    // commands remain perfectly runnable. So the test is the commands: if they are
                    // still findable under the module's name, the module is usable and the rows
                    // that belong to it stay. Only when the import fails AND the commands are gone
                    // is the module actually unloadable.
                    int remaining = 0;
                    try
                    {
                        using var check = PowerShell.Create();
                        check.Runspace = runspace;
                        check.AddCommand("Get-Command").AddParameter("Module", name);
                        remaining = check.Invoke().Count;
                    }
                    catch { }

                    if (remaining > 0)
                    {
                        continue;
                    }

                    _session.Emit(EventKinds.Verification, ConsoleColor.Red, ConsoleColor.Black,
                        name + "|" + error);
                }
            }
        }
        catch (Exception ex)
        {
            _session.Emit(EventKinds.Verification, ConsoleColor.Red, ConsoleColor.Black,
                "verifier|The verification scan failed: " + ex.Message);
        }
        finally
        {
            try { runspace?.Dispose(); } catch { }
        }
    }
}