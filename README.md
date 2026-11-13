# PS7 ISE

An open-source clone of the Windows PowerShell ISE (`powershell_ise.exe`) that runs
**PowerShell 7** instead of Windows PowerShell 5.1. The front-end is Rust on native Win32
controls; the shell itself runs in a child .NET process that hosts a real `PSHost` and
runspace, so the console pane behaves like a console: prompt, inline output, colours,
history, `Ctrl+C`, and Tab completion from the shell's own `TabExpansion2`.

![layout](docs/layout.png)

The window is the original ISE arrangement: menu bar, toolbar, a tabbed script pane on
top, the console pane below it, and a status bar showing the current location.

## Requirements

| | To build | To run `dist` |
| --- | --- | --- |
| Windows | x64 | x64 |
| Rust | stable toolchain with the MSVC builder | — |
| .NET | SDK 10.0 or newer | .NET runtime of the generation the pinned PowerShell targets (.NET 10 for PowerShell 7.6) |

`build.ps1 -SelfContained` bundles the .NET runtime into `dist`, which removes the .NET
requirement on the machine that runs it, at the cost of a much larger folder.

## Build

`dist\` is committed, so a fresh clone is runnable as it is: start `dist\PS7_ISE.exe` and
the console pane comes up on the PowerShell pinned in the csproj. To rebuild after changing
anything:

```powershell
.\build.ps1                 # engine + front-end into .\dist
.\build.ps1 -SkipEngine     # front-end only (leaves the published engine alone)
.\build.ps1 -SkipFrontEnd   # engine only (keeps the PS7_ISE.exe already in dist)
.\build.ps1 -SelfContained  # ship the .NET runtime in dist (about 200 MB)
```

Then run `dist\PS7_ISE.exe`. The engine files have to stay next to the exe: PowerShell
derives `$PSHome`, and therefore `$PSModulePath`, from the folder containing
`System.Management.Automation.dll`, so `dist\Modules\` is what makes the bundled cmdlets
load.

Scripts open from the command line as well - `dist\PS7_ISE.exe demo.ps1` starts with that
file in the script pane - or from File > Open Script, `Ctrl+O`, inside the app.

## Which PowerShell you get

The version is pinned by one property in
`PowerShellEngineHost\PowerShellEngineHost\PowerShellEngineHost.csproj`:

```xml
<PowerShellVersion>7.6.10</PowerShellVersion>
<TargetFramework>net10.0</TargetFramework>
```

`Microsoft.PowerShell.SDK` **is** the PowerShell that runs - the app never looks for an
installed `pwsh.exe`, so what you build is what you get. The banner in the console pane is
`$PSVersionTable.PSVersion` read straight out of that assembly, and `$PSHome` points at
`dist`. Check it with `$PSVersionTable` at the prompt.

The goal is the newest stable release on the newest .NET it supports. A brand new minor
release is deliberately left alone until it has had a few patches - `7.7.0` is skipped for
now, on the grounds that Microsoft finds bugs faster than anyone else does.

To move to another release, raise `PowerShellVersion` and set `TargetFramework` to the
framework that release is built for - `net8.0` for 7.4, `net9.0` for 7.5, `net10.0` for
7.6. `build.ps1` finds the module folder by pattern, so nothing else refers to the
framework.

## Console keys

| Key | Effect |
| --- | --- |
| `Enter` | submit the line |
| `Tab` | completions from `TabExpansion2`; repeated Tab cycles the candidates |
| `Up` / `Down` | command history |
| `Ctrl+C` | copy the selection if there is one, otherwise stop the running command / abandon the line |
| `Ctrl+V`, `Ctrl+A`, `Esc` | paste, jump to the start of the line, clear the current line |
| `PgUp` / `PgDn` | scroll the scroll-back |
| `F5` / `F8` | run the script pane / run the selected lines |
| `F1` | PowerShell 7 help |
| `Ctrl+N` `Ctrl+O` `Ctrl+S` `Ctrl+Shift+S` `Ctrl+W` | new, open, save, save as, close the script tab |

## How it works

```
PS7_ISE.exe  (Rust, native-windows-gui)
    |  writes:  L <line>  T <caret> <text>  S  X
    |  reads:   E <kind> <fg> <bg> <text>   C <count> <text>   Q <code>
    v
PowerShellEngineHost.exe  (.NET, Microsoft.PowerShell.SDK)
    Program.cs   the protocol
    ISEHost.cs   PSHost + PSHostRawUserInterface
    Session.cs   the runspace, the prompt loop, TabExpansion2
```

One record per line, with text escaped to printable ASCII (`\\`, `\uXXXX`), so nothing a
command prints can break the framing. The Rust side keeps the console document in memory -
scroll-back plus the line being typed - and pushes it into the rich edit control once per
timer tick. Colour is remembered as runs and replayed with `EM_SETCHARFORMAT` after each
refresh, because the one write the control reliably accepts, `WM_SETTEXT`, carries plain
text only.

The engine is a separate process on purpose: hosting .NET in-process through `hostfxr`
needs `load_assembly_and_get_function_pointer`, and every entry point that touches
`Microsoft.PowerShell.SDK` fails to resolve (`0x80131509`). Native AOT is not an option
either - `System.Management.Automation` reflects over itself and dies at the first
pipeline. A child process also means a runaway command cannot take the UI down with it.

## Debugging

- `PS7ISE_DEBUG=1` traces engine events to `%TEMP%\ps7ise.log`, and makes the app replay
  whatever is written to `%TEMP%\ps7ise-inject.txt` as keystrokes - the way automated tests
  drive the prompt, since Windows refuses to deliver input posted by another process to a
  window without the keyboard focus. `{MENU:n}` in that file runs the menu command with id
  `n` (see `state::cmd`), which covers the entries that have no key of their own.
- The engine's own stderr goes to `%TEMP%\ps7ise-engine.log`.

## Repository layout

| Path | Contents |
| --- | --- |
| `src\main.rs` | window, menus, toolbar, layout, timer pump, shortcuts |
| `src\state.rs` | application state, console document and colour runs, engine pump |
| `src\console.rs` | the console rich edit control: text, selection, colours, caret |
| `src\input.rs` | console keystrokes, history, completion insertion, clipboard |
| `src\docs.rs` | script tabs, file I/O, "run this in the console" |
| `src\highlight.rs` | PowerShell syntax colouring for the script pane |
| `src\engine.rs` | the child process and its record protocol |
| `PowerShellEngineHost\` | the .NET engine |
| `build.ps1` | publishes the engine, flattens its modules, builds the front-end |

## Known limitations

- Windows x64 only; the UI is Win32 controls, so there is no theming beyond the console's
  own colours.
- No script debugger: no breakpoints, no stepping, no variable inspection. The Debug menu
  holds only the three commands it can really perform - Run Script, Run Selection, Stop Run.
- Panes split at a fixed ratio and there is no draggable splitter bar; View > Show/Hide
  switches between one pane and two.
- The console scroll-back is trimmed once it passes about 400 000 characters, and every
  refresh rewrites the document, which a normal session never notices but a command
  printing hundreds of megabytes will.
- Windows PowerShell 5.1 is deliberately not supported.

## License

MIT.
