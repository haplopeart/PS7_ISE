using System;
using System.IO;

/// <summary>
/// Decides whether Start-Job can work in this build, and says so when it cannot.
///
/// A background job is a second PowerShell process, and PowerShell starts it at
/// <c>$PSHome\pwsh.exe</c>. <c>$PSHome</c> is derived from the folder holding this engine and
/// PowerShell keeps it read-only - assigning to it throws - so the job's location is not
/// something a host can choose.
///
/// The <c>pwsh.exe</c> that the SDK package puts next to the engine is a self-contained
/// apphost: it looks for its runtime in its own folder and nowhere else, so jobs need
/// <c>coreclr.dll</c> there too. A self-contained build (<c>build.ps1 -SelfContained</c>,
/// which is what the installer ships) has it, and jobs run with nothing installed on the
/// machine. A framework-dependent build does not, and there is nothing to do about it from
/// in here: <c>Program Files</c> is read-only for a normal user, and copying a runtime into
/// any other folder would not help, because the apphost above does not search there. An
/// earlier revision copied a whole PowerShell install beside the engine on first run; that
/// cost 1100 files and a slow first start and still left every job failing, so it is gone.
/// </summary>
internal static class JobHost
{
    /// <summary>
    /// Returns what the user should be told about background jobs, or an empty string when
    /// there is nothing to say. Never throws: a build without jobs still edits and runs
    /// scripts, and a session that cannot warn about jobs must not fail to start.
    /// </summary>
    public static string Ensure()
    {
        try
        {
            string home = AppContext.BaseDirectory;

            if (!File.Exists(Path.Combine(home, "pwsh.exe")))
            {
                return "Start-Job will not work here: there is no pwsh.exe next to PS7_ISE.exe, "
                    + "and a background job is a second PowerShell process.";
            }

            // pwsh.exe is a self-contained apphost, so coreclr.dll beside it is what tells us
            // the job can actually start. hostfxr.dll and hostpolicy.dll come with it: the job
            // asks for them in its own folder rather than in the machine-wide runtime.
            if (File.Exists(Path.Combine(home, "coreclr.dll")))
            {
                return "";
            }

            return "Start-Job will not work here: pwsh.exe carries its own .NET runtime and "
                + "this build has none beside it. The installer ships a build that does; a "
                + "folder built without -SelfContained cannot run background jobs.";
        }
        catch (Exception ex)
        {
            return "Could not check whether Start-Job works here: " + ex.Message;
        }
    }
}
