# Project rules

## PowerShell version policy

- Ship the **newest stable PowerShell 7 on its newest supported .NET**, currently
  `7.6.10` on `net10.0` (.NET 10). Do not drop to an older framework to make a build
  easier.
- **Do not chase a release on the day it lands.** A fresh minor release stays unpicked
  until it has had patches: `7.7.0` is deliberately deferred, it goes in a later build
  once Microsoft has shipped fixes for it.
- The single source of truth is `<PowerShellVersion>` in
  `PowerShellEngineHost\PowerShellEngineHost\PowerShellEngineHost.csproj`. Bumping it
  means also setting `<TargetFramework>` to the framework that release is built for
  (`net8.0` for 7.4, `net9.0` for 7.5, `net10.0` for 7.6/7.7) - a mismatch is a restore
  error, not a warning. Nothing else in the app names a version: `build.ps1` finds the
  module folder by pattern.
- After a bump, `.\build.ps1` and re-commit `dist\`. It is tracked on purpose so a clone
  is runnable, so a version change is not done until `dist\` matches the csproj.

## Do not resurrect

Hosting .NET **in-process** through `hostfxr` was tried and abandoned: every
`UnmanagedCallersOnly` entry point that touches `Microsoft.PowerShell.SDK` fails to
resolve (`0x80131509`), and Native AOT is closed off because `System.Management.Automation`
reflects over itself. The engine stays a child process.

`EM_REPLACESEL`, `EM_SETTEXTEX`, `EM_STREAMIN`, `WM_PASTE` and friends are all no-ops on
the console control in this environment. `WM_SETTEXT` is the only write that works, which
is why the console document lives in memory and colour is replayed per run after every
refresh.

## Verifying by hand

RichEdit content cannot be read reliably from another process - `GetWindowTextW`,
`WM_GETTEXT` and `EM_GETTEXTLENGTHEX` all lie here. Use a `PrintWindow` screenshot, or the
in-process log with `PS7ISE_DEBUG=1`.

`cargo` is not on `PATH` in this shell; prefix with `C:\Users\haplo\.cargo\bin`. The
release build must stay warning-free.
