# PS7 ISE Cross-Platform Port Plan (macOS first, Linux-ready)

**Status:** Feasibility spike + prototype direction  
**Date:** 2026-11-21 (updated after local pull to v1.4.0)  
**Target repo:** `haplopeart/PS7_ISE`  
**Local clone:** `~/Documents/GitHub/PS7_ISE`

## 1. Objective

Evaluate feasibility of a macOS release of PS7 ISE and produce a practical path to a prototype, while keeping the architecture OS-agnostic so the same codebase can later target Windows, macOS, and Linux.

This document covers:

1. **Feasibility spike** - what exists today, what blocks macOS, what can be reused.
2. **Recommended approach** - engine-first, UI-rewrite with a shared cross-platform core.
3. **Prototype plan** - a new macOS UI prototype that speaks the existing engine protocol, designed as a cross-platform UI foundation.

---

## 2. Current-state assessment (evidence)

### 2.1 What the app is today

From `README.md`, `Cargo.toml`, and source inspection:

| Area | Current implementation | Portability |
|---|---|---|
| Frontend | Rust binary `PS7_ISE`, `native-windows-gui` 1.0 with `rich-textbox`, `menu`, `file-dialog`, `tabs` | **Windows-only** |
| UI rendering | Win32 RichEdit (`EM_*`, `CHARFORMAT2`), GDI/DWM/comctl32, window subclassing, Win32 menus/dialogs/status bar | **Windows-only** |
| Build/resources | `build.rs` uses `winresource`, `rc.exe`, Windows manifest/icon; `build.ps1` publishes Windows dist | **Windows-only** |
| Engine | .NET 10 / `Microsoft.PowerShell.SDK` 7.6.10, separate process, stdio line protocol | **Mostly cross-platform** |
| Engine protocol | Line-based records `L/T/M/S/P/X` in, `E/C/M/Q` out; escaped printable ASCII text | **Cross-platform by design** |
| Packaging | NSIS installer, `.exe`, SmartScreen/signing notes | **Windows-only** |

### 2.2 Code size / coupling snapshot

Approx. LOC (from `wc -l`):

- Rust frontend: ~7,916 LOC across `src/*.rs`
- .NET engine: ~2,632 LOC across `PowerShellEngineHost/*.cs`

Windows coupling indicators:

- `src/` has 200+ references to `nwg`/Win32/SendMessage/HWND/EM_* APIs.
- `build.rs` compiles Windows resources and embeds a Windows manifest.
- `Cargo.toml` has only `native-windows-gui` and `winresource`.
- Engine has Windows P/Invoke in `NativeConsole.cs` (`kernel32.dll` CreatePipe/SetStdHandle/CloseHandle) and `NativeOutputPump.cs` (GetOEMCP, SafeFileHandle).
- Engine csproj pins `RuntimeIdentifier=win-x64`.

### 2.3 Reusable cross-platform core

These parts are good reuse candidates:

1. **Protocol contract** - already process-isolated, text-based, testable (`engine-protocol-test.ps1` exists).
2. **Engine session/host/metadata logic** - `Session.cs`, `ISEHost.cs`, `CommandMetadata.cs`, `Program.cs` mostly use PowerShell SDK abstractions.
3. **PowerShell hosting model** - "real PSHost/runspace in child process" is portable and is a good safety boundary.
4. **Product behavior** - console semantics, command metadata, tab completion, history, prompt loop; can be preserved at the protocol/UI-state layer.

### 2.4 Major blockers

| Blocker | Why it matters | Severity |
|---|---:|---|
| `native-windows-gui` frontend | Entire UI framework is Win32-specific; no macOS/Linux backend | Critical |
| RichEdit console implementation | `console.rs` depends on Win32 rich text control behavior, EM_ messages, CHARFORMAT2 | Critical |
| Win32 input/subclassing | `input.rs`, `main.rs` use subclassing/message handling for console behavior | High |
| Windows resources/theme | `build.rs`, `theme.rs`, `icons.rs`, `images.rs`, DWM/comctl32 | High |
| Native console capture in engine | `NativeConsole.cs`/`NativeOutputPump.cs` use kernel32 P/Invoke | Medium |
| Windows build/package scripts | `build.ps1`, NSIS installer, win-x64 publish layout | Medium |
| Windows-only assumptions in UX | `%TEMP%` paths, Start Menu, Windows-specific module filtering/wording | Medium |

**Conclusion:** A native macOS release is feasible, but not by compiling existing frontend as-is. The right move is a cross-platform UI rewrite over the existing engine protocol.

---

## 3. Recommended approach

## Guiding principle

> **Keep the engine/process boundary; replace the Windows UI with an OS-agnostic UI shell and shared app core.**

This aligns with the existing architecture and minimizes risk: PowerShell hosting remains isolated; UI becomes portable.

### 3.1 Target architecture

```text
+--------------------------------------------------------------+
|                 Cross-platform UI Shell                      |
|  (macOS / Linux / Windows)                                   |
|  - window/layout/menu/dialogs/theme                          |
|  - script editor tabs                                        |
|  - console view (colored runs + input)                       |
|  - commands panel                                            |
+------------------------------+-------------------------------+
                               |
                               v
+--------------------------------------------------------------+
|                 Shared App Core (Rust or .NET)               |
|  - document state, console buffer, history, completion UI    |
|  - protocol client, engine lifecycle, event pump             |
|  - command metadata model, settings, logging                 |
+------------------------------+-------------------------------+
                               | stdio line protocol (stable contract)
                               v
+--------------------------------------------------------------+
|          PowerShellEngineHost (.NET, cross-platform)          |
|  - PSHost/Runspace                                           |
|  - TabExpansion2, command metadata, execution, cancellation  |
|  - native child-process capture (platform-specific impl)     |
+--------------------------------------------------------------+
```

### 3.2 OS-agnostic strategy

To avoid repeating this work for Linux/Windows later:

1. **Define a stable engine protocol spec** (versioned, documented, tested with golden fixtures).
2. **Isolate platform-specific code behind small interfaces**:
   - native process/pty/pipe capture in engine
   - file dialogs, clipboard, global shortcuts, theme integration in UI
   - install/update packaging per OS
3. **Keep UI state/model independent of widget toolkit**:
   - console buffer + color runs
   - document/tab model
   - command metadata view-model
   - keybinding/command dispatch
4. **Choose a cross-platform UI toolkit** for prototype/foundation, not a macOS-only one.
5. **Run CI on Windows/macOS/Linux** for engine protocol tests and core logic.

### 3.3 UI toolkit options (for prototype/foundation)

| Option | Cross-platform | Pros | Cons | Fit |
|---|---|---|---|---|
| **Tauri v2 (Rust + system webview)** | Win/macOS/Linux | OS-agnostic, good packaging story, can reuse Rust core, web editor ecosystem | Webview differences; terminal/console fidelity needs care | Good long-term |
| **Avalonia (.NET)** | Win/macOS/Linux | Single language with engine, good desktop controls | Rewrite UI in .NET; team/tooling shift; no dotnet currently installed in this env | Good long-term |
| **egui/eframe (Rust)** | Win/macOS/Linux | Fast to prototype, pure Rust, easy custom console rendering | Immediate-mode UI quirks; native menus/dialog polish varies; large text/editor ergonomics need work | **Best spike/prototype here** |
| **Swift/AppKit** | macOS only | Best native Mac UX | Not OS-agnostic; conflicts with Linux reuse goal | Not recommended |
| **Qt** | Win/macOS/Linux | Mature desktop widgets | Licensing/size/bindings complexity | Possible |

**Prototype choice:** **egui/eframe** for speed and OS-agnostic foundation. It can prove protocol integration, console rendering, and app structure quickly. The prototype should isolate core state/protocol from egui so we can swap to Tauri/Avalonia later if needed.

---

## 4. Feasibility by workstream

### 4.1 Engine (high feasibility)

**Can reuse now:** protocol shape, Session/ISEHost/CommandMetadata/Program structure, PowerShell SDK hosting.

**Changes needed:**

- Add platform-conditional native output capture:
  - Windows: existing CreatePipe/SetStdHandle/GetOEMCP.
  - Unix/macOS/Linux: `pipe`, `dup2`, `fcntl`, UTF-8/locale-aware decoding, SIGCHLD-safe process handling as needed.
- Make csproj publish cross-platform:
  - remove hard `win-x64` default for all builds, or add conditional RIDs.
  - support `osx-arm64`, `osx-x64`, `linux-x64`, `linux-arm64`.
- Replace Windows-only build assumptions in `build.ps1` with cross-platform script (`build.sh`/`Justfile`/`dotnet` commands).
- Validate `Start-Job`/job host behavior on macOS/Linux; document platform differences.
- Add engine smoke tests on macOS/Linux.

**Risk:** native command capture parity and terminal semantics.  
**Mitigation:** keep capture simple initially; add PTY-backed mode later if needed.

### 4.2 Frontend (medium feasibility, high effort)

**Do not port `native-windows-gui` incrementally.** Treat as rewrite.

**Build in layers:**

1. **App Core** - protocol client, event bus, console buffer, document model, command metadata model.
2. **UI Shell** - tabs, editor, console, commands panel, menus, status bar.
3. **Platform adapters** - file dialogs, clipboard, paths, fonts/theme defaults, packaging.

**Must-have parity for MVP:**

- Start engine, show banner/prompt
- Execute command, stream colored output
- Input, Enter, Ctrl+C, history, Tab completion
- Script open/save/run selection/run script
- Commands panel basic browse/insert/run
- Clear console, exit

### 4.3 Packaging/release (medium feasibility)

- macOS: `.app` bundle, codesign, notarization, DMG/pkg.
- Linux: tarball/AppImage/deb/rpm (later), desktop entry, icon/theme integration.
- Windows: keep existing NSIS path initially; later migrate to shared packaging pipeline.

---

## 5. Prototype plan (Item 3)

## 5.1 Prototype goal

Build a **cross-platform UI prototype** that runs on macOS now and can be extended to Linux/Windows, proving:

- UI can speak the existing engine protocol.
- Console output/color/events render correctly.
- Basic input/command execution/completion loop works.
- Core UI state is separated from platform/toolkit-specific code.

## 5.2 Prototype non-goals

- Full ISE parity.
- Full debugger.
- Full Commands panel parity.
- Production installer/signing.
- Perfect RichEdit behavior.

## 5.3 Proposed prototype structure

Create `cross-platform-ui/` (Rust + eframe/egui) with OS-agnostic modules:

```text
cross-platform-ui/
  Cargo.toml
  README.md
  src/
    main.rs              # eframe bootstrap only
    app.rs               # UI composition / event loop
    protocol.rs          # engine record parser/encoder (portable)
    engine_client.rs     # child process stdio client (portable)
    console_model.rs     # color runs, scrollback, prompt/input model
    commands_model.rs    # metadata reply parsing/model (initial)
    mock_engine.rs       # protocol-speaking mock for dev/tests
    theme.rs             # palette abstraction
    platform/
      mod.rs             # platform paths/config abstraction
```

**Design rule:** no egui types in `protocol.rs`, `engine_client.rs`, `console_model.rs`. UI crate can be swapped later.

## 5.4 Prototype milestones

| Milestone | Deliverable | Acceptance |
|---|---|---|
| P0 - Scaffold | Cross-platform Rust app launches on macOS | Window opens, basic layout visible |
| P1 - Protocol core | Parser/encoder for `E/C/M/Q` and `L/T/S/P/X` | Unit tests pass with fixture records |
| P2 - Console MVP | Colored output buffer, prompt/input line, history | Can run commands via mock engine; Ctrl+C/Enter/history work |
| P3 - Real engine hookup | Spawn real engine when available; fallback to mock | Works with mock now; clear error/instructions when engine missing |
| P4 - Script/editor stub | Minimal script editor pane and Run Selection/Run Script commands | Sends commands through protocol/client |
| P5 - Cross-platform hardening | Path/config abstraction, no macOS-only APIs in core | Same code compiles on macOS; Linux/Windows build checks documented |

## 5.5 Immediate implementation in this spike

I will implement P0-P3 (and as much P4 as feasible) in-repo:

- Add `cross-platform-ui/` prototype.
- Implement protocol parser/encoder + tests.
- Implement mock engine speaking protocol.
- Implement egui UI with console, input, history, status, command send.
- Try to connect to real engine if present; otherwise use mock and report clearly.
- Document how to run on macOS and what is needed for Linux/Windows.

---

## 6. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Console fidelity (RichEdit behavior hard to replicate) | UX regressions | Build console model as semantic runs; add golden protocol tests; iterate on input edge cases |
| Toolkit choice lock-in | Future rewrite cost | Keep core model/protocol UI-agnostic; thin UI adapter |
| Engine native capture differences on Unix | Missing native command output / hangs | Platform-specific capture module; feature flag; PTY fallback plan |
| PowerShell module/platform differences | Commands behave differently on macOS/Linux | Surface platform; keep module verification; document unsupported modules |
| No .NET SDK in current environment | Can't build real engine now | Prototype with mock engine + protocol compatibility; document exact SDK/install steps |

---

## 7. Validation plan

### Engine/protocol

- Unit tests for protocol parser/encoder.
- Golden transcript tests using recorded `E/C/M/Q` lines.
- Reuse/adapt `engine-protocol-test.ps1` for macOS/Linux once engine builds.

### UI prototype

- Manual smoke: launch, prompt, run command, stderr color, completion, history, Ctrl+C, clear.
- `cargo test` for protocol/model.
- `cargo run` on macOS.

### Cross-platform discipline

- No `#[cfg(target_os = "macos")]` in core modules except platform adapter.
- CI matrix target: macOS, Linux, Windows for core + engine.

---

## 8. Recommended next steps after prototype

1. **Engine macOS/Linux port** - implement Unix native console capture, cross-platform publish, smoke tests.
2. **Protocol spec document** - formalize record grammar, escaping, chunking, busy/error behavior.
3. **Decide production UI toolkit** - evaluate egui prototype vs Tauri/Avalonia against fidelity/performance/accessibility.
4. **Build shared app core crate/package** - extract from prototype.
5. **Implement MVP parity** - editor tabs, run script/selection, commands panel, settings, logging.
6. **Packaging/signing** - macOS notarized app first, then Linux packages, keep Windows.

---

## 9. Deliverables from this spike

- This plan: `docs/proposals/macos-linux-port-plan.md`
- Prototype: `cross-platform-ui/` (cross-platform Rust/egui, protocol-aware)
- Prototype run/test evidence captured in terminal output during implementation.

---

## 10. Update after local pull to v1.4.0

The local clone was updated to `v1.4.0` (commit `348b092`, tag `v1.4.0`). The feasibility conclusion is unchanged, but the port scope increased slightly:

### New/changed areas observed

- **Debugger integration** (`PowerShellEngineHost/PowerShellEngineHost/DebugService.cs`, Session/Program changes, UI changes in `src/main.rs`, `src/console.rs`, `src/input.rs`, `harness/test_debugger.ps1`).
- **New protocol surface**:
  - Frontend -> engine: `D <verb> [arg]` (debug commands such as breakpoint set/list/clear, resume/step; see `DebugService.cs` and `Program.cs` case `D`).
  - Engine -> frontend: `D s ...` (stopped), `D r` (resumed), and chunked debug replies via `M b ...` / `M k ...` framing (`WriteChunked` reused for breakpoints/call stack).
- **UI behavior additions**: right-click copy/paste fixes and debugger groundwork/UI.

### Impact on port plan

- **Engine**: DebugService uses PowerShell runspace debugger APIs - likely cross-platform in principle, but must be included in macOS/Linux engine smoke tests.
- **Protocol**: extend protocol spec/tests to cover `D` records and `M b/k` chunked replies. Prototype parser currently treats unknown `D` records as `Foreign` (safe fallback) but should gain typed support.
- **UI prototype**: debugger UX (breakpoints, step controls, stack/variables panes) is a post-MVP parity item; keep protocol/model hooks OS-agnostic.
- **Plan adjustment**: add "Debugger protocol parity" milestone after console/editor/commands MVP.

### Verification

- `cross-platform-ui` prototype tests re-run after pull: **6 passed, 0 failed** (`cargo test`).

---

## 11. Suggested next steps - implementation status (this session)

| Step | Status | Evidence / artifacts | Notes |
|---|---|---|---|
| 1. Engine macOS/Linux port | **In progress (macOS build/smoke verified; Linux not runtime-verified here)** | `NativeConsole.cs` Unix pipe/dup2/fcntl path, `NativeOutputPump.cs` cross-platform fallback, `PowerShellEngineHost.csproj` conditional RID/WINDOWS, `Session.cs` macOS ExecutionPolicy/STA guard, `build.sh` | macOS arm64: `dotnet build` 0 errors/1 pre-existing warning; smoke run returns banner/prompt, executes command, `rc 0`; `engine-protocol-test.ps1` **62/62 PASS** after cross-platform test fixes. Unix native stdout/stderr capture verified on macOS; `Start-Job` fails in published layout because `pwsh` not adjacent (known packaging/job-host limitation). Linux runtime/CI execution blocked locally (no docker). |
| 2. Protocol spec | **Done (draft)** | `docs/proposals/protocol-spec.md`, `docs/proposals/protocol-fixtures.jsonl`, prototype `protocol.rs` D-record support/tests | Spec derived from v1.4.0 behavior; validate against engine on Windows/macOS/Linux. |
| 3. Harden prototype core | **Done (prototype level)** | `cross-platform-ui/src/{protocol,console_model,metadata_model,commands_model,settings,logging,engine_client}.rs`, 13 passing tests | Core/UI boundaries improved; can be extracted into separate crate/library next. |
| 4. UI shell evaluation | **Done (initial)** | `docs/proposals/ui-shell-evaluation.md` | Recommends keeping OS-agnostic core; evaluate Tauri vs Avalonia for production MVP. |
| 5. MVP parity pass | **Partial/prototype** | `cross-platform-ui/src/app.rs` commands panel (index+detail request), debug pane (breakpoints/stack/step controls), native file dialogs via `rfd`, selectable console with Copy All, run/stop/history/completion; 16 cargo tests passing. | Not full ISE parity. Full debugger parity (variables, enable/disable breakpoint, source nav, stop-while-running breakpoint queueing), RichEdit-like colored+selectable console, full commands panel parity, packaging/signing/notarization/CI release still pending. |

### Verification commands run

```bash
cd cross-platform-ui && cargo test
# result: 13 passed, 0 failed
```

### Blockers / environment limits

- `dotnet` SDK is not installed on this machine, so engine cross-platform build/runtime smoke tests could not be executed locally.
- macOS notarization/signing/packaging not performed (requires Apple Developer credentials and release pipeline).
- Full ISE parity (debugger UI, commands panel parity, RichEdit console semantics) is substantial follow-up work.
