# PS7 ISE Engine Protocol Spec (Draft)

**Status:** Draft, derived from v1.4.0 implementation  
**Scope:** Stdio line protocol between UI frontend and `PowerShellEngineHost`  
**Source of truth:** `PowerShellEngineHost/PowerShellEngineHost/Program.cs`, `Session.cs`, `DebugService.cs`, `src/engine.rs`

## 1. Transport

- Frontend spawns engine as child process.
- **Frontend -> Engine:** UTF-8 lines on engine stdin.
- **Engine -> Frontend:** UTF-8 lines on engine stdout.
- Engine stderr is diagnostic/log output, not protocol.
- One record per line. Records must not contain raw newline characters.

## 2. Text encoding

All `<text>` / payload fields use printable ASCII escaping:

- `\` -> `\\`
- printable ASCII `0x20..0x7e` -> literal
- everything else -> `\uXXXX` (UTF-16 code unit hex, lowercase/uppercase accepted by decoder)

Decoders should be tolerant of malformed/truncated escapes (drop/replace rather than crash).

## 3. Frontend -> Engine commands

| Record | Format | Meaning |
|---|---|---|
| Submit line | `L <text>` | Submit a line/command at prompt |
| Tab completion | `T <caret> <text>` | Request TabExpansion2 completions; `caret` is integer |
| Metadata | `M i` | Request command index |
| Metadata refresh | `M r` | Invalidate/rebuild command index |
| Metadata detail | `M d <name>` | Request detail for command name |
| Stop | `S` | Interrupt running command (Ctrl+C) |
| Redraw prompt | `P` | Reprint prompt (after clear) |
| Exit | `X` | Shut down session |
| Debug | `D <verb> [arg]` | Debugger command (see §6) |

## 4. Engine -> Frontend records

| Record | Format | Meaning |
|---|---|---|
| Output event | `E <kind> <fg> <bg> <text>` | Colored console output/event |
| Completions | `C <count> <text>` | Completion items separated by U+001E; empty payload allowed |
| Metadata/debug chunk | `M <kind> <part> <of> <reply> <payload>` | Chunked reply; payload may contain spaces |
| Quit | `Q <code>` | Session ended |

### 4.1 Event kinds (`E`)

| Kind | Name | Notes |
|---:|---|---|
| 1 | Output | Normal output |
| 2 | Error | Error output |
| 3 | Warning | Warning |
| 4 | Verbose | Verbose |
| 5 | Debug | Debug stream |
| 6 | Progress | Progress; UI may render specially |
| 7 | Title | Window/title update |
| 8 | CommandStarted | Command/pipeline started; UI busy=true |
| 9 | CommandFinished | Command/pipeline finished; UI busy=false |
| 10 | InputRequested | Prompt/input requested; UI busy=false, input_requested=true |
| 11 | InputEcho | Input echo |
| 12 | Verification | Module verification verdict (background) |

`fg`/`bg` are integer RGB packed as `0xRRGGBB` decimal in current implementation.

### 4.2 Completions (`C`)

- `count` is decimal item count.
- `text` is encoded payload containing completion items separated by U+001E.
- Decoder should also tolerate empty/whitespace quirks.

### 4.3 Chunked `M` replies

Format: `M <kind> <part> <of> <reply> <payload>`

- `kind`: `i` index, `d` detail, `b` breakpoints, `k` call stack, `e` error/refusal.
- `part`: 1-based chunk number.
- `of`: total chunks; >=1.
- `reply`: monotonically increasing reply id per answer stream.
- `payload`: encoded text chunk; may include spaces. Parser must split into 6 fields max.

Reassembly: collect chunks for same `(kind, reply, of)` until all parts seen; concatenate in part order.

Interleaving: `E`/`C`/`D`/other records may appear between `M` chunks. Receivers must not discard partial `M` state on unrelated records.

### 4.4 Quit (`Q`)

`Q <code>` indicates engine/session exit. Frontend should flush remaining records and close stdin.

## 5. Debug protocol (`D`) - v1.4.0+

Frontend sends `D <verb> [arg]`.

| Verb | Example | Meaning |
|---|---|---|
| `b + <line> <path>` | `D b + 42 C:\a.ps1` | Set line breakpoint |
| `b - <id>` | `D b - 3` | Remove breakpoint |
| `b e <id>` | `D b e 3` | Enable breakpoint |
| `b d <id>` | `D b d 3` | Disable breakpoint |
| `b c` | `D b c` | Clear breakpoints |
| `b l` | `D b l` | List breakpoints |
| `k` | `D k` | Request call stack |
| `c` | `D c` | Continue |
| `i` | `D i` | Step into |
| `o` | `D o` | Step over |
| `u` | `D u` | Step out |
| `q` | `D q` | Stop debugging/runspace execution as defined by engine |

Engine responses:

- `D s <reason> <line> <col> <encodedPath>` - debugger stopped/hit breakpoint.
- `D r` - debugger resumed.
- Chunked `M b ...` - breakpoint rows.
- Chunked `M k ...` - call stack rows.

*Note:* exact payload field semantics should be validated against `DebugService.cs` during port; this spec captures observed v1.4.0 behavior.

## 6. Error/refusal behavior

- Metadata/debug requests may be refused when runspace is busy; engine may answer with `M e <part> <of> <reply> <text>`.
- Unknown commands should be ignored safely.
- Malformed records should not crash engine/UI; log and continue.

## 7. Compatibility rules

- Additive changes preferred: new `E` kinds, new `M` kinds, new `D` verbs.
- Unknown `E` kind -> treat as generic output/unknown.
- Unknown `M` kind -> preserve raw or ignore, but do not corrupt stream state.
- Unknown top-level record -> log as foreign; do not terminate session.

## 8. Golden fixtures

See `docs/proposals/protocol-fixtures.jsonl` for sample records and expected parsed structures. Fixtures are used by prototype tests and should be reused by engine port smoke tests.
