# UI Shell Evaluation (Cross-Platform PS7 ISE)

**Date:** 2026-11-21  
**Context:** Suggested next step 4 - choose production UI shell.

## Candidates

| Option | Cross-platform | Fidelity/UX | Dev speed | Packaging | Verdict |
|---|---|---|---|---|---|
| **egui/eframe (prototype)** | Win/macOS/Linux | Medium for desktop IDE; custom console doable but text editing/accessibility/native menus need work | Fast | Good (Rust binary) | **Good prototype/core validation; risky as final full-ISE shell** |
| **Tauri v2 (Rust core + webview)** | Win/macOS/Linux | Good with Monaco/CodeMirror/xterm-like components; webview consistency work needed | Medium | Good | **Strong candidate for production** |
| **Avalonia (.NET)** | Win/macOS/Linux | Good desktop controls; editor/terminal controls need selection | Medium | Good | **Strong candidate if staying .NET-centric** |
| **Swift/AppKit + separate Windows/Linux UI** | No (macOS only) | Excellent native Mac | Slow overall | Mac-native | Reject for OS-agnostic goal |
| **Qt** | Win/macOS/Linux | Mature | Medium/slow | Heavy/licensing considerations | Possible fallback |

## Recommendation

1. **Keep the prototype's OS-agnostic core** (`protocol`, `console_model`, `metadata_model`, `engine_client`, settings/logging) as the reusable base.
2. For production MVP, evaluate **Tauri v2** vs **Avalonia** with a spike:
   - Tauri if we want to reuse Rust core and web editor ecosystem.
   - Avalonia if we want one .NET stack with engine.
3. Do not commit to egui for full ISE parity until editor/console/accessibility/native integration gaps are proven acceptable.

## Criteria for final decision

- Console fidelity (colored output, prompt editing, selection/copy, history, completion, Ctrl+C)
- Script editor quality (syntax highlighting, selection, tabs, large files)
- Accessibility and IME
- Native menus/dialogs/shortcuts
- Packaging/signing/notarization on macOS, Linux packaging, Windows installer
- Team skill fit and maintenance cost
