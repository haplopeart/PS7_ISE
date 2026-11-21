# PS7 ISE Cross-Platform UI Prototype

A small, OS-agnostic UI prototype for the PS7_ISE port effort. It is intentionally **not** full ISE parity; it proves the cross-platform direction:

- protocol parser/encoder compatible with existing PS7_ISE engine protocol
- console model separated from UI toolkit
- engine client abstraction (real process or mock)
- egui/eframe UI that runs on macOS/Linux/Windows

## Run

```bash
cd cross-platform-ui
cargo run
```

## Use a real engine

If/when a cross-platform `PowerShellEngineHost` binary is built:

```bash
PS7ISE_ENGINE=/path/to/PowerShellEngineHost cargo run
```

The prototype will also look for engine candidates under the repo `dist/` and engine build output directories.

## Without a real engine

If no engine is found, the prototype starts a **mock protocol engine** that executes commands through local `pwsh` (`-NoProfile -NonInteractive -Command`) and emits protocol-compatible output events. This makes the UI demonstrable on macOS today.

## Tests

```bash
cargo test
```

## Design boundaries (OS-agnostic)

- `protocol.rs` - no UI types; pure record parsing/encoding, including `D` debug records.
- `console_model.rs` - no UI types; console buffer/history/completion state.
- `metadata_model.rs` - chunked `M` reply reassembly.
- `commands_model.rs` - command index/detail model for Commands panel.
- `engine_client.rs` - process/mock transport; stdio protocol.
- `settings.rs` / `logging.rs` - simple persistent settings and file logging.
- `platform/mod.rs` - OS-specific paths/behavior only.
- `app.rs` - egui adapter; replaceable by Tauri/Avalonia/native shell later.

## MVP prototype features

- Console output with colored runs, prompt/input, history, tab completion, Ctrl+C stop, clear.
- Script pane with run script, basic open/save hooks (`PS7ISE_OPEN`/`PS7ISE_SAVE` env vars), Ctrl+Enter run fallback.
- Commands panel (requests `M i`, displays/filter/selects command names, insert selected).
- Debug record awareness (`D s`/`D r`) in protocol/core.
- Mock engine fallback via local `pwsh` for demonstrable macOS runs.

## Known prototype limitations

- Not full ISE parity (no full debugger UI, basic editor/selection behavior, limited file dialogs).
- Console uses simple run rendering, not a terminal emulator/RichEdit equivalent.
- Tab completion uses mock `T` behavior in mock mode; real engine provides real TabExpansion2.
- Real engine needs platform-port validation on macOS/Linux (Unix native capture code added, but .NET SDK not available in this environment for runtime verification).
