using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;

/// <summary>
/// Makes Start-Job work in a folder that has the PowerShell libraries but no PowerShell.
///
/// A background job is a second PowerShell process, and PowerShell looks for it at
/// <c>$PSHome\pwsh.exe</c>. Publishing the SDK puts every assembly next to the engine except that
/// one, because the SDK is a library rather than the application, so every job died with "The pwsh
/// executable cannot be found" - and since <c>Get-WindowsUpdate</c> runs its scan plus its log
/// conversion in jobs, lines 1 and 2 of a maintenance script that works in the original ISE came
/// back empty here.
///
/// The fix copies the application in from an installed PowerShell 7 on first run, into this very
/// folder, because a job resolves its assemblies from the folder holding pwsh.exe and that folder
/// is already the engine's $PSHome. Only files missing here are added, so the assemblies published
/// at the pinned version are never displaced by another patch of the same library, and the copy is
/// refused unless the installed PowerShell has the same major and minor as the engine: pwsh.dll
/// from 7.5 must not be loaded against System.Management.Automation from 7.6.
/// </summary>
internal static class JobHost
{
    private const string EngineMajorMinor = "7.6";
    // Diagnostics path: write to temp so a read-only install directory does not fail.
    private static string Trace => Path.Combine(Path.GetTempPath(), ".jobhost.trace");

    /// <summary>
    /// Returns what the user should be told about jobs, or null when there is nothing to say.
    /// Never throws: a machine without PowerShell 7 simply carries on without jobs.
    /// </summary>
    public static (string notice, string jobHome) Ensure()
    {
        try
        {
            // A background job runs the PowerShell that sits in its own $PSHome, and the
            // job's apphost looks for hostfxr.dll and hostpolicy.dll there. The install
            // directory is read-only for a normal user, so the job host lives in the
            // user's own AppData: copying it there is what makes Start-Job work on a
            // freshly installed machine, and the runspace is told to use the same folder
            // so the job can find pwsh.exe.
            string home = JobHome();
            // A background job runs the PowerShell that sits next to this process, and the
            // job's apphost looks for hostfxr.dll and hostpolicy.dll in its own folder. The
            // build ships pwsh.exe but not those two - the SDK's flat publish leaves them
            // out and the PowerShell install keeps them - so "pwsh.exe is present" is not
            // enough: the job has to be able to start, which means the loader DLLs too.
            bool hasLoader = File.Exists(Path.Combine(home, "hostfxr.dll"))
                && File.Exists(Path.Combine(home, "hostpolicy.dll"));
            if (File.Exists(Path.Combine(home, "pwsh.exe")) && hasLoader)
            {
                return ("", home);
            }

            string installed = FindInstalled();
            if (installed == null)
            {
                return ("Start-Job needs pwsh.exe next to PS7_ISE.exe and no PowerShell 7 install "
                    + "was found to copy it from, so background jobs will not run here.", "");
            }

            if (!MatchesVersion(installed))
            {
                return ("Found PowerShell " + GetMajorMinor(installed)
                    + ", but this app was built for " + EngineMajorMinor + ". Jobs may fail.", "");
            }

            int added = CopyJobHost(Path.GetDirectoryName(installed), home);
            // The copy walks the PowerShell install tree and brings every leaf into the job
            // folder, including hostfxr.dll and hostpolicy.dll. They sit in the same folder as
            // pwsh.exe, and the job's apphost - the one that ships with PowerShell - asks for
            // them in its own folder rather than in the machine-wide shared directory, so the
            // job needs them. They break the engine's own start, whose apphost was built from
            // the SDK and resolves the runtime through the registry instead, so they are
            // skipped during the copy and restored from the install afterwards: the job runs
            // with them, the engine does not.
            RestoreLoader(Path.GetDirectoryName(installed), home);
            return ("Started a PowerShell " + GetMajorMinor(installed) + " job host from "
                + installed + " (" + added + " files added).", home);
        }
        catch (Exception ex)
        {
            return ("Could not install a job host for Start-Job: " + ex.Message, "");
        }
    }

    /// <summary>
    /// Where the job host lives. The install directory is read-only for a normal user, so
    /// the job host is copied into the user's own AppData, where it can grow: a background
    /// job resolves its assemblies from $PSHome, and $PSHome is pointed at this folder.
    /// </summary>
    private static string JobHome()
    {
        return Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "HCW", "PS7ISE", "JobHost");
    }

    private static string GetMajorMinor(string path)
    {
        try
        {
            var v = FileVersionInfo.GetVersionInfo(path);
            string[] parts = v.FileVersion.Split('.');
            if (parts.Length >= 2)
            {
                return parts[0] + "." + parts[1];
            }
        }
        catch
        {
        }
        return "?";
    }

    private static bool MatchesVersion(string path)
    {
        string mm = GetMajorMinor(path);
        return mm == EngineMajorMinor;
    }

    private static string FindInstalled()
    {
        foreach (string candidate in new[]
        {
            @"C:\Program Files\PowerShell\7\pwsh.exe",
            @"C:\Program Files\PowerShell\7-preview\pwsh.exe",
            Environment.ExpandEnvironmentVariables(@"%LocalAppData%\Microsoft\WindowsApps\pwsh.exe"),
        })
        {
            if (string.IsNullOrEmpty(candidate) || !File.Exists(candidate))
            {
                continue;
            }
            try
            {
                var info = FileVersionInfo.GetVersionInfo(candidate);
                if (info.ProductMajorPart >= 7 && info.ProductMajorPart < 8)
                {
                    return candidate;
                }
            }
            catch
            {
            }
        }
        return null;
    }

    /// <summary>Copies the job host into <paramref name="destDir"/> if it is not already there.</summary>
    private static int CopyJobHost(string sourceDir, string destDir)
    {
        int added = 0;
        try
        {
            // The PowerShell install root contains reparse points / junctions that cause
            // Directory.GetFiles / DirectoryInfo.GetFiles to throw for the whole tree even
            // though individual files inside them would be fine. Walk the tree ourselves
            // instead and treat each leaf independently.
            WalkAndCopy(sourceDir, destDir, ref added);
            return added;
        }
        catch (Exception ex)
        {
            File.AppendAllText(Trace, $"copy failed: {ex}\n");
            return added;
        }
    }

    private static void WalkAndCopy(string source, string dest, ref int added)
    {
        Directory.CreateDirectory(dest);
        foreach (string file in SafeEnumerateFiles(source))
        {
            string name = Path.GetFileName(file);
            // The .NET host loader. The job's apphost asks for these in its own folder, and
            // the PowerShell install has its own copies, so they are skipped here and copied
            // back afterwards by RestoreLoader: the job needs them, the engine does not.
            if (name.Equals("hostfxr.dll", StringComparison.OrdinalIgnoreCase)
                || name.Equals("hostpolicy.dll", StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }

            string target = Path.Combine(dest, name);
            if (!File.Exists(target))
            {
                try { File.Copy(file, target, false); added++; }
                catch (Exception ex)
                {
                    File.AppendAllText(Trace, $"skip file {name}: {ex.GetType().Name}\n");
                }
            }
        }
        foreach (string sub in SafeEnumerateDirectories(source))
        {
            WalkAndCopy(sub, Path.Combine(dest, Path.GetFileName(sub)), ref added);
        }
    }

    /// <summary>
    /// Puts the host loader DLLs back in <paramref name="destDir"/> from the PowerShell
    /// install. The job's apphost looks for them there, while the engine's own apphost
    /// resolves the runtime through the registry and refuses them, so the copy happens after
    /// the job host is in place and before anything in this folder starts.
    /// </summary>
    private static void RestoreLoader(string sourceDir, string destDir)
    {
        foreach (string dll in new[] { "hostfxr.dll", "hostpolicy.dll" })
        {
            string source = Path.Combine(sourceDir, dll);
            string target = Path.Combine(destDir, dll);
            if (!File.Exists(source) || File.Exists(target))
            {
                continue;
            }
            try
            {
                File.Copy(source, target, false);
            }
            catch (Exception ex)
            {
                File.AppendAllText(Trace, $"skip loader {dll}: {ex.GetType().Name}\n");
            }
        }
    }

    private static IEnumerable<string> SafeEnumerateFiles(string dir)
    {
        List<string> result = new();
        try
        {
            foreach (string f in Directory.EnumerateFiles(dir)) { result.Add(f); }
        }
        catch (Exception ex)
        {
            File.AppendAllText(Trace, $"cannot read dir {dir}: {ex.GetType().Name}\n");
        }
        return result;
    }

    private static IEnumerable<string> SafeEnumerateDirectories(string dir)
    {
        List<string> result = new();
        try
        {
            foreach (string d in Directory.EnumerateDirectories(dir)) { result.Add(d); }
        }
        catch (Exception ex)
        {
            File.AppendAllText(Trace, $"cannot list dirs {dir}: {ex.GetType().Name}\n");
        }
        return result;
    }
}
