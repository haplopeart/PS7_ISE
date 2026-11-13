//! The PowerShell 7 engine, hosted in a child process.
//!
//! `PS7_ISE.exe` starts `PowerShellEngineHost.exe` (the .NET project under
//! `PowerShellEngineHost\`, which runs whatever `Microsoft.PowerShell.SDK` version that
//! project pins), which hosts a real `PSHost`/`PSHostUserInterface` pair and a
//! runspace. Commands travel to it as single lines on standard input, coloured output
//! comes back as single lines on standard output, so the console pane behaves like the
//! original ISE console: prompt, inline output, TabExpansion2 completions and Ctrl+C.
//!
//! The record format is documented in `PowerShellEngineHost\Program.cs`. Text fields are
//! escaped to printable ASCII, which keeps a record on one line no matter what the shell
//! prints.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// The published engine next to the front-end.
pub const ENGINE_EXE: &str = "PowerShellEngineHost.exe";
/// Engine diagnostics (its standard error) land here.
const ENGINE_LOG: &str = "ps7ise-engine.log";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How long to wait for the runspace to come up before giving up.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(45);
/// Tab expansion may have to autoload a module, so it gets a generous window.
const COMPLETION_TIMEOUT: Duration = Duration::from_secs(6);
/// Bound on queued output, so a chatty command cannot eat all the memory.
const MAX_QUEUED_EVENTS: usize = 50_000;

/// What a [`PsEvent`] means to the console pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventKind {
    Output,
    Error,
    Warning,
    Verbose,
    Debug,
    Progress,
    Title,
    CommandStarted,
    CommandFinished,
    InputRequested,
    InputEcho,
    Unknown,
}

impl EventKind {
    fn from_i32(v: i32) -> Self {
        match v {
            1 => EventKind::Output,
            2 => EventKind::Error,
            3 => EventKind::Warning,
            4 => EventKind::Verbose,
            5 => EventKind::Debug,
            6 => EventKind::Progress,
            7 => EventKind::Title,
            8 => EventKind::CommandStarted,
            9 => EventKind::CommandFinished,
            10 => EventKind::InputRequested,
            11 => EventKind::InputEcho,
            _ => EventKind::Unknown,
        }
    }
}

/// One coloured run of text coming out of the shell.
#[derive(Clone, Debug)]
pub struct PsEvent {
    pub kind: EventKind,
    pub fg: [u8; 3],
    /// Background as the engine painted it. The console has one fixed background for the
    /// whole document, so the value is parsed for completeness and never applied.
    #[allow(dead_code)]
    pub bg: [u8; 3],
    pub text: String,
}

fn rgb(mut value: i64) -> [u8; 3] {
    if value < 0 {
        value = 0;
    }
    let r = ((value >> 16) & 0xff) as u8;
    let g = ((value >> 8) & 0xff) as u8;
    let b = (value & 0xff) as u8;
    [r, g, b]
}

/// Everything the reader thread learns about the engine, shared with the UI thread.
#[derive(Default)]
struct EngineState {
    /// Output waiting for the next timer tick.
    events: VecDeque<PsEvent>,
    /// Reply to the pending `T` request, if one has arrived.
    completions: Option<Vec<String>>,
    /// True once the shell is gone: the input line has to close.
    ended: bool,
    /// True between CommandStarted and CommandFinished.
    busy: bool,
    /// True while the shell waits for a line of input.
    input_requested: bool,
    /// The engine's exit code once it is over.
    exit_code: i32,
    /// Set by the first record: the engine is alive and talking.
    ready: bool,
}

/// Handle on the hosted engine. All calls come from the UI thread.
pub struct Engine {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    state: Arc<(Mutex<EngineState>, Condvar)>,
    log_path: PathBuf,
}

impl Engine {
    /// Starts `PowerShellEngineHost.exe` from `engine_dir`.
    pub fn load(engine_dir: &Path) -> Result<Engine, String> {
        let exe = engine_dir.join(ENGINE_EXE);
        if !exe.is_file() {
            return Err(format!(
                "Engine host not found: {} - the published engine files must sit next to \
                 PS7_ISE.exe.",
                exe.display()
            ));
        }

        let log_path = std::env::temp_dir().join(ENGINE_LOG);
        let stderr = std::fs::File::create(&log_path)
            .map_err(|e| format!("Could not open {}: {}", log_path.display(), e))?;

        let mut command = Command::new(&exe);
        command
            .current_dir(engine_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr));
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);

        let mut child = command
            .spawn()
            .map_err(|e| format!("Failed to start {}: {}", exe.display(), e))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Could not write to the engine's standard input".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Could not read the engine's standard output".to_string())?;

        let state = Arc::new((Mutex::new(EngineState::default()), Condvar::new()));
        let reader_state = Arc::clone(&state);
        std::thread::Builder::new()
            .name("ps7ise-engine-reader".to_string())
            .spawn(move || read_loop(stdout, reader_state))
            .map_err(|e| format!("Could not start the engine reader thread: {}", e))?;

        Ok(Engine {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            state,
            log_path,
        })
    }

    /// Waits until the engine has produced its first output, which means the runspace is
    /// open and the first prompt is on its way.
    pub fn start(&mut self) -> Result<(), String> {
        match self.wait_for(|state| state.ready || state.ended, STARTUP_TIMEOUT) {
            Some(state) if state.ready => Ok(()),
            Some(state) => Err(format!(
                "The PowerShell engine ended before it was ready (exit code {}). See {}",
                state.exit_code,
                self.log_path.display()
            )),
            None => Err(format!(
                "The PowerShell engine did not respond within {} seconds. See {}",
                STARTUP_TIMEOUT.as_secs(),
                self.log_path.display()
            )),
        }
    }

    /// Submits one line at the prompt, as if Enter had been pressed.
    pub fn execute(&mut self, line: &str) {
        self.send(&format!("L {}\n", encode(line)));
    }

    /// Interrupts the running command (Ctrl+C).
    pub fn stop(&mut self) {
        self.send("S\n");
    }

    /// Asks the shell to print its prompt again, used after the console pane was cleared:
    /// the prompt was part of the text that was just thrown away.
    pub fn redraw_prompt(&mut self) {
        self.send("P\n");
    }

    /// TabExpansion2 completions for `line` with the caret at `caret`.
    pub fn completions(&mut self, line: &str, caret: usize) -> Vec<String> {
        if let Ok(mut state) = self.state.0.lock() {
            state.completions = None;
        }

        self.send(&format!("T {} {}\n", caret, encode(line)));

        match self.wait_for(|state| state.completions.is_some() || state.ended, COMPLETION_TIMEOUT) {
            Some(mut state) => state.completions.take().unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// Drains pending output. `limit` bounds how much work one UI tick may do.
    pub fn drain_events(&mut self, limit: usize) -> Vec<PsEvent> {
        let Ok(mut state) = self.state.0.lock() else {
            return Vec::new();
        };
        (0..limit).filter_map(|_| state.events.pop_front()).collect()
    }

    pub fn is_ended(&self) -> bool {
        self.state.0.lock().map(|s| s.ended).unwrap_or(true)
    }

    pub fn exit_code(&self) -> i32 {
        self.state.0.lock().map(|s| s.exit_code).unwrap_or(0)
    }

    /// Asks the engine to shut down, then reaps it.
    pub fn shutdown(&mut self) {
        self.send("X\n");

        let Ok(mut guard) = self.child.lock() else { return };
        let Some(child) = guard.as_mut() else { return };

        // Give the engine a moment to tear its runspace down on its own.
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
                _ => break,
            }
        }

        let _ = child.kill();
        let _ = child.wait();
    }

    /// Blocks until `predicate` holds, the timeout expires or the pipe closes.
    /// The returned guard lets the caller take the value it was waiting for.
    fn wait_for<F>(&self, predicate: F, timeout: Duration) -> Option<std::sync::MutexGuard<'_, EngineState>>
    where
        F: Fn(&EngineState) -> bool,
    {
        let (lock, changed) = &*self.state;
        let mut guard = lock.lock().ok()?;
        let deadline = Instant::now() + timeout;
        while !predicate(&guard) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let (next, wait) = match changed.wait_timeout(guard, remaining) {
                Ok(pair) => pair,
                Err(_) => return None,
            };
            guard = next;
            if wait.timed_out() && !predicate(&guard) {
                return None;
            }
        }
        Some(guard)
    }

    fn send(&self, payload: &str) {
        let Ok(mut guard) = self.stdin.lock() else { return };
        let Some(stdin) = guard.as_mut() else { return };
        // A broken pipe just means the engine already went away.
        let _ = stdin.write_all(payload.as_bytes()).and_then(|_| stdin.flush());
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Reads engine records until the pipe closes, updating shared state as they arrive.
fn read_loop(stdout: std::process::ChildStdout, state: Arc<(Mutex<EngineState>, Condvar)>) {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }

        let record = line.trim_end_matches(|c| c == '\n' || c == '\r');
        if record.is_empty() {
            continue;
        }

        let Ok(mut guard) = state.0.lock() else { break };
        guard.ready = true;

        let mut fields = record.splitn(5, ' ');
        match fields.next() {
            Some("E") => {
                let kind = fields.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                let fg = fields.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0xcc_cc_cc);
                let bg = fields.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                let text = fields.next().unwrap_or("");

                match kind {
                    8 => {
                        guard.busy = true;
                        guard.input_requested = false;
                    }
                    9 => {
                        guard.busy = false;
                    }
                    10 => {
                        guard.busy = false;
                        guard.input_requested = true;
                    }
                    _ => {}
                }

                if guard.events.len() >= MAX_QUEUED_EVENTS {
                    guard.events.pop_front();
                }
                guard.events.push_back(PsEvent {
                    kind: EventKind::from_i32(kind),
                    fg: rgb(fg),
                    bg: rgb(bg),
                    text: decode(text),
                });
            }

            Some("C") => {
                let _count = fields.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                let payload = fields.next().unwrap_or("");
                guard.completions = Some(
                    decode(payload)
                        .split('\u{1e}')
                        .filter(|item| !item.is_empty())
                        .map(|item| item.to_string())
                        .collect(),
                );
            }

            Some("Q") => {
                guard.exit_code = fields.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                guard.ended = true;
                guard.busy = false;
                guard.input_requested = false;
            }

            // Anything else is not ours; a foreign line must not end the session.
            _ => continue,
        }

        drop(guard);
        state.1.notify_all();
    }

    // Standard output closed: the shell is gone, so the input line has to close too.
    if let Ok(mut guard) = state.0.lock() {
        guard.ended = true;
        guard.busy = false;
        guard.input_requested = false;
    }
    state.1.notify_all();
}

/// Turns text into a single printable ASCII field, matching the engine's encoding.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    for unit in value.encode_utf16() {
        match unit {
            0x5c => out.push_str("\\\\"),
            0x20..=0x7e => out.push(unit as u8 as char),
            other => out.push_str(&format!("\\u{:04x}", other)),
        }
    }
    out
}

/// Reverses [`encode`]. Lone surrogates from a truncated escape become U+FFFD, which is
/// better than dropping the rest of the record.
fn decode(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }

    let bytes = value.as_bytes();
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;

    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'\\' => {
                    units.push(b'\\' as u16);
                    i += 2;
                    continue;
                }
                b'u' => {
                    if let Some(hex) = value.get(i + 2..i + 6) {
                        if let Ok(code) = u16::from_str_radix(hex, 16) {
                            units.push(code);
                            i += 6;
                            continue;
                        }
                    }
                }
                _ => {}
            }
        }
        units.push(byte as u16);
        i += 1;
    }

    String::from_utf16_lossy(&units)
}
