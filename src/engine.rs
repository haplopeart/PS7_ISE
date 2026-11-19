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

use crate::commands::{MetadataReply, MetadataStream};
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
/// Bound on command-metadata answers waiting to be read. The panel asks for one thing at a
/// time, so this only ever holds the answers it had not got to yet - and a Refresh pressed
/// while a list is still arriving can produce several.
const MAX_QUEUED_METADATA: usize = 8;

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
    /// The `M` answer being stitched together as its chunks arrive. Nothing else can do
    /// this: a command that finishes while a list is being read puts its output between two
    /// chunks, so the records only make sense to something that remembers the answer so far.
    metadata: MetadataStream,
    /// Finished `M` answers, oldest first, waiting for the Commands panel to pick them up.
    metadata_ready: VecDeque<MetadataReply>,
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

        // The engine is published framework-dependent and the machine does not ship the exact
        // canonical patch of Microsoft.NETCore.App that the runtimeconfig.json names. Without
        // this the host refuses to start with "No frameworks were found", and the console pane
        // would be dead on every machine that is not freshly provisioned. The env var is only
        // set here - it must not be set globally, or the front-end's own .NET lookups change.
        #[cfg(windows)]
        command.env("DOTNET_ROLL_FORWARD", "LatestMinor");

        // The job host restores hostfxr.dll and hostpolicy.dll into this folder whenever a
        // background job runs, because the job's own apphost asks for them there. The engine
        // is framework-dependent and its apphost resolves the runtime through the registry,
        // so those two files make it refuse to start. Strip them every time, before spawn.
        #[cfg(windows)]
        strip_loader(engine_dir);

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

    /// Asks for every command the session can see (`M i`). The answer is not waited for: it is
    /// four thousand names spread over several records, and the UI thread cannot stop for that.
    pub fn read_commands(&mut self) {
        self.send("M i\n");
    }

    /// Asks for the same list with the engine's cache of it thrown away (`M r`). Importing a
    /// module adds commands to the session, so the panel needs a way to say "look again".
    pub fn reread_commands(&mut self) {
        self.send("M r\n");
    }

    /// Asks for one command's syntax and parameters (`M d <name>`). The name goes out escaped
    /// because a function can be called almost anything.
    pub fn read_command(&mut self, name: &str) {
        self.send(&format!("M d {}\n", encode(name)));
    }

    /// Takes the command-metadata answers that have arrived, without waiting for one. The
    /// panel asks on a click and finds the answer on a later tick, which is the only way to
    /// keep a second-long read from freezing the window.
    pub fn drain_metadata(&mut self, limit: usize) -> Vec<MetadataReply> {
        let Ok(mut state) = self.state.0.lock() else {
            return Vec::new();
        };
        (0..limit)
            .filter_map(|_| state.metadata_ready.pop_front())
            .collect()
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
        if crate::state::debug_enabled() {
            // Only the verb and the length: a submitted line can hold a password the user
            // typed at the prompt.
            crate::state::log_line(&format!(
                "engine <: {} ({} chars)",
                payload.split(' ').next().unwrap_or("?"),
                payload.chars().count()
            ));
        }
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

            Some("M") => {
                // Six fields, not five. This payload is the sixth and, unlike every other
                // text field on the wire, it contains spaces of its own that the engine's
                // escaping leaves alone - splitting it with the shared five would cut the
                // answer off at the first one. So it is split again, with room for it.
                let mut meta = record.splitn(6, ' ');
                meta.next();
                let kind = meta.next().unwrap_or("");
                let part = meta.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                let of = meta.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                let reply = meta.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                let payload = meta.next().unwrap_or("");

                if let Some(done) = guard.metadata.feed(kind, part, of, reply, &decode(payload)) {
                    if guard.metadata_ready.len() >= MAX_QUEUED_METADATA {
                        guard.metadata_ready.pop_front();
                    }
                    guard.metadata_ready.push_back(done);
                }
            }

            Some("Q") => {
                guard.exit_code = fields.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                guard.ended = true;
                guard.busy = false;
                guard.input_requested = false;
                // Whatever list was on its way is not coming.
                guard.metadata.reset();
            }

            // Anything else is not ours, and a foreign line must not end the session.
            //
            // This is where a native command's text lands when the engine did not capture
            // it: `DISM.exe` and `MpCmdRun.exe` write to the standard handle they inherited
            // from the engine, which is this very pipe. Logged rather than ignored quietly,
            // because "the command ran but never said anything" is otherwise impossible to
            // tell apart from a command that genuinely had nothing to report.
            _ => {
                if crate::state::debug_enabled() {
                    crate::state::log_line(&format!("engine: foreign line dropped: {}", record));
                }
                continue;
            }
        }

        drop(guard);
        state.1.notify_all();
    }

    // Standard output closed: the shell is gone, so the input line has to close too.
    if let Ok(mut guard) = state.0.lock() {
        guard.ended = true;
        guard.busy = false;
        guard.input_requested = false;
        // A command list that was half way here is never going to be finished, and the panel
        // has to be able to tell that case apart from one it is still waiting for.
        guard.metadata.reset();
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

/// Removes the host loader DLLs from `engine_dir` when the engine is framework-dependent,
/// which is where the engine and the job host live. The job host restores them when a
/// background job runs, because the job's own apphost asks for them in its folder; the
/// framework-dependent engine's apphost resolves the runtime through the registry, so they
/// make it refuse to start. A self-contained publish is the opposite case - it bundles
/// coreclr.dll and refuses to run without those two in its own folder - so the decision is
/// read from the engine's runtimeconfig.json rather than from how it was built.
///
/// Removing a file that is not there is not an error, and a file that is in use is left
/// alone: the next start will fail with the message the host itself gives.
#[cfg(windows)]
fn strip_loader(engine_dir: &Path) {
    let rc = engine_dir.join("PowerShellEngineHost.runtimeconfig.json");
    let framework_dependent = match std::fs::read_to_string(&rc) {
        Ok(text) => !text.contains("includedFrameworks"),
        Err(_) => true,
    };
    if !framework_dependent {
        return;
    }
    for dll in ["hostfxr.dll", "hostpolicy.dll"] {
        let path = engine_dir.join(dll);
        if path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
    }
}
