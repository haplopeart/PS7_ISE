# Debugger: where this left off

Handed over on 2026-11-21, at a pause to reboot the machine. Working tree was clean and
`dotnet build -c Release` reported 0 errors (3 warnings - see "Warnings" below, which violates
the warning-free rule in AGENTS.md and needs a look).

## Bug to fix first: debugger answers travel as command-metadata records

`PowerShellEngineHost\Program.cs` has `WriteChunked(kind, payload, reply)` - the chunker the
debugger calls - and it emits **`M <kind> <part> <of> <reply> <text>`**, the command-metadata
record. Two results:

1. The front end has one `MetadataStream` (`src\engine.rs` `EngineState::metadata`, fed at the
   `Some("M")` arm). A variables list arriving while the Commands panel is indexing four
   thousand names lands inside that reply and both are wrong afterwards.
2. The kinds the debugger uses (`b` breakpoints, `k` call stack, `l` variables, `p` evaluate)
   are not ones `src\commands.rs` knows, so every debugger answer is dropped anyway.

The design said the debug verb would be `D` with its own reply counter for exactly this
reason; the implementation reused the `M` framing by copy-paste.

Fix, in order:

- `Program.cs`: give the chunker a record letter - `WriteChunked(string verb, string kind,
  string payload, int reply)` - and have it write `verb` instead of the literal `"M "`.
  Then `WriteCommandMetadata` ends with `WriteChunked("M", kind, payload, reply)` (its
  `kind == "v"` single-chunk fast path and the surrogate-pair comment stay where they are),
  and the debugger's calls gain the `"D"` first argument.
- `DebugService.cs`: five call sites, `Handle` and friends -
  `Program.WriteChunked("D", "b", ...)`, `("D", "k", ...)`, `("D", "l", ...)`, `("D", "p", ...)`.
- `Program.cs`: document `D` in the protocol header at the top of the file, as `L`, `T`, `S`,
  `X`, `M` are already documented.

## Then the front end, which has no debugger at all

Engine side is real: `DebugService.cs` answers over `D`, `Session.cs` attaches it before the
first prompt and exposes `TryQueueDebugRequest` / `RequestDebugResume` / `DebugError`, and
`Program.cs` has the `case 'D':` verb. Nothing in `src\` can drive it - grep for
`breakpoint|StepInto|StepOver` in `src\*.rs` returns nothing. So the shipped app still has no
breakpoints, no stepping, no variable display, and the README's "No script debugger"
limitation is still the truth.

Order that fits the existing code:

1. `src\engine.rs` - a `Some("D")` arm with six fields, split like the `Some("M")` arm does
   (the payload has spaces of its own, so it needs `splitn(6, ' ')`, not the shared five);
   its own reassembly struct so it cannot touch `metadata`. Also `D s <reason> <line> <col>
   <script>` for the stop and `D r` for the resume, both plain records. Add `debug_command()`
   to send `D <verb> <encoded argument>`, and a `drain_debug()`.
2. `src\debug.rs` (new) - reassembly plus row parsing. Rows are separated by `U+001E`, fields
   by `U+001F`, the same marks `src\commands.rs` uses.
3. `src\state.rs` - command ids past `RUN_AS_ADMIN = 42` (a 50s block reads well for debug),
   and state for the breakpoint list, the stop location, the variables and the call stack.
4. `src\gutter.rs` - a dot per breakpoint, a marker for the current line, and a click in the
   strip to toggle. `paint()` and `line_at()` already know the rows.
5. `src\main.rs` - the Debug menu already exists (`build_menus`, the `&Debug` header, with
   Run Script / Run Selection / Stop Run); add Toggle Breakpoint `F9`, Continue `F5`, Step
   Into `F11`, Step Over `F10`, Step Out `Shift+F11`, Stop Debugger `Shift+F5`. `shortcut()`
   is where the keys are matched, `run_command()` is where ids are dispatched, and Ctrl+C
   (`input::stop_execution`) has to ask for a debugger resume rather than a plain `S` while a
   stop is outstanding.
6. Variables and Call Stack panes, then a version bump and release.

Constraints worth knowing: a line breakpoint needs a path that exists on disk, so an unsaved
tab cannot carry one (`DebugService.SetOne` says so); debug mode is set to
`LocalScript|RemoteScript` only while at least one breakpoint exists, because leaving it on
costs real speed on ordinary prompt use.

## Push backlog (needs the network)

`git push origin main` is owed four commits: `382b2d5`, `1475dd8`, `4562963`, `265b401`.
`v1.3.0` and `v1.4.0` exist as pushed **tags** but have **no GitHub release** behind them, so
`releases/latest` does not point at either and no installer is attached. `v1.4.1` has no tag.
`gh` is not logged in here; the token comes from
`"protocol=https`nhost=github.com`n`n" | git credential fill` used as a one-shot `GH_TOKEN`,
and `gh release create` still needs `git tag` + `git push origin <tag>` afterwards.

## Warnings

The Release build reported 3 warnings. One is `DebugService.cs` CS0162 (unreachable code - a
`break` after `return` in the breakpoint switch). AGENTS.md wants the release build
warning-free, so clear all three before the next release.
