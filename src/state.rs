//! Mutable application state plus everything the console pane can be asked to do.
//!
//! native-windows-gui hands control access to `'static` closures, so the state lives in
//! one heap allocation reached through a pointer. Every access happens on the UI thread.

use crate::console::{normalize_breaks, ConsoleView};
use crate::engine::{Engine, EventKind};
use std::collections::HashMap;
use std::path::PathBuf;

pub const CONSOLE_FG: [u8; 3] = [0xCC, 0xCC, 0xCC];
pub const ERROR_FG: [u8; 3] = [0xE7, 0x48, 0x56];
pub const DIM_FG: [u8; 3] = [0x9B, 0x9B, 0x9B];
pub const CONSOLE_BG: [u8; 3] = [0x0C, 0x0C, 0x0C];
const MAX_CONSOLE_CHARS: i32 = 400_000;
const KEEP_CONSOLE_CHARS: i32 = 250_000;
const MAX_CONSOLE_RUNS: usize = 4000;
const CONSOLE_FONT: &str = "Consolas";
const CONSOLE_HEIGHT: i32 = 220; // twips: 11pt

/// A stretch of console text the engine wanted in a particular colour. `start` and `end`
/// are UTF-16 code unit offsets into `AppState::console_buffer`, which is exactly how the
/// rich edit control counts, so they can be handed to it unchanged.
#[derive(Clone, Copy)]
pub struct ConsoleRun {
    pub start: i32,
    pub end: i32,
    pub fg: [u8; 3],
}

#[link(name = "user32")]
extern "system" {
    fn MessageBeep(kind: u32) -> i32;
    pub fn InvalidateRect(hwnd: *mut core::ffi::c_void, rect: *const i32, erase: i32) -> i32;
    fn UpdateWindow(hwnd: *mut core::ffi::c_void) -> i32;
}

pub fn beep() {
    unsafe {
        MessageBeep(0);
    }
}

/// One open script.
pub struct Doc {
    pub title: String,
    pub path: Option<PathBuf>,
    pub text: String,
}

/// Identifier for a menu item or toolbar button, so one handler can dispatch them all.
pub mod cmd {
    pub type Id = u32;

    pub const NEW: u32 = 1;
    pub const OPEN: u32 = 2;
    pub const SAVE: u32 = 3;
    pub const SAVE_AS: u32 = 4;
    pub const CLOSE_TAB: u32 = 5;
    pub const EXIT: u32 = 6;

    pub const CUT: u32 = 10;
    pub const COPY: u32 = 11;
    pub const PASTE: u32 = 12;

    pub const RUN_SCRIPT: u32 = 20;
    pub const RUN_SELECTION: u32 = 21;
    pub const STOP: u32 = 22;

    pub const CLEAR_CONSOLE: u32 = 30;
    pub const TOGGLE_SCRIPT_PANE: u32 = 31;
    pub const TOGGLE_CONSOLE_PANE: u32 = 32;
    pub const TOGGLE_COMMANDS: u32 = 33;
    pub const REFRESH_COMMANDS: u32 = 34;

    pub const HELP: u32 = 40;
    pub const ABOUT: u32 = 41;
    /// Relaunch the program with Administrator privileges, like the shield icon on the
    /// original ISE's Run button.
    pub const RUN_AS_ADMIN: u32 = 42;

    // The debugger. F5 is not in this list: it belongs to Run Script and is routed by whether
    // the shell is held at a stop, which is what the original's single Run/Continue button did.
    pub const TOGGLE_BREAKPOINT: u32 = 50;
    pub const CONTINUE: u32 = 51;
    pub const STEP_INTO: u32 = 52;
    pub const STEP_OVER: u32 = 53;
    pub const STEP_OUT: u32 = 54;
    pub const STOP_DEBUGGING: u32 = 55;
    pub const CLEAR_BREAKPOINTS: u32 = 56;
    pub const TOGGLE_DEBUGGER: u32 = 57;
}

/// Everything the UI needs at run time.
pub struct AppState {
    pub window: nwg::Window,
    pub tabs: nwg::TabsContainer,
    pub pages: Vec<nwg::Tab>,

    /// The text each tab header shows, kept in step with `pages`. A tab control has no
    /// way to drop a header it has been given, so the strip is updated in place - new tab,
    /// renamed tab - rather than rebuilt, and this is what makes a rename detectable.
    pub tab_titles: Vec<String>,
    pub editor: nwg::RichTextBox,
    pub console: nwg::RichTextBox,
    pub status: nwg::StatusBar,
    pub console_view: Option<ConsoleView>,

    /// Narrow strip to the left of the script pane that carries its line numbers, like the
    /// edit window of the original ISE. A rich edit has no gutter of its own, so this is a
    /// window of `gutter`'s own window class, which paints the numbers from the pane's rows
    /// - see [`crate::gutter`].
    pub line_gutter: Option<*mut core::ffi::c_void>,
    /// The font those numbers are drawn in, smaller than the script's, the way a margin
    /// reads. The strip borrows it rather than owning it, so it has to stay alive.
    #[allow(dead_code)]
    pub gutter_font: Option<nwg::Font>,
    /// The caret position text in the status bar's right-hand panel, e.g. "Line 12, Col 5".
    pub caret_text: String,
    /// Top row, line count and strip height the line numbers were painted for, so the
    /// update timer only invalidates the strip when one of them moved.
    pub gutter_key: (i32, i32, i32, u64),

    /// Kept alive: destroying a menu or button a handler still refers to would crash
    /// the message loop. They are never read after construction, which is the point.
    #[allow(dead_code)]
    pub menus: Vec<nwg::Menu>,
    #[allow(dead_code)]
    pub menu_items: Vec<nwg::MenuItem>,
    pub items: Vec<(cmd::Id, nwg::ControlHandle)>,
    #[allow(dead_code)]
    pub buttons: Vec<nwg::Button>,
    pub button_items: Vec<(cmd::Id, nwg::ControlHandle)>,
    pub _font: Option<nwg::Font>,
    /// The 40 ms pump. Held for the lifetime of the window; nothing reads it back.
    #[allow(dead_code)]
    #[allow(deprecated)]
    pub timer: Option<nwg::Timer>,

    /// The engine is started on the first timer tick so the window appears at once.
    pub engine_pending: bool,

    /// 0.0 = script pane only, 1.0 = console only.
    pub split: f32,
    pub show_script: bool,
    pub show_console: bool,
    /// The Commands column down the right-hand side, like the original ISE's. Shown from the
    /// start: it is what the pane is for, and hiding it by default would leave the feature
    /// undiscoverable.
    pub show_commands: bool,
    /// What the Commands column knows, and the controls that show it.
    pub commands: crate::panel::Commands,

    /// The Debugger column, under the Commands one: the frames a stopped script is held
    /// between, what those frames hold, and an Immediate box. See [`crate::debug_pane`].
    pub debugger: crate::debug_pane::Debugger,
    /// Hidden until there is something to debug. Unlike the Commands column, this one has
    /// nothing to show while the shell is running normally, and a stop opens it by itself —
    /// which is when it gets asked for.
    pub show_debugger: bool,
    /// What the shell has said about breakpoints and about the stop, if there is one.
    pub debug: crate::debug::Model,
    /// Held so the font outlives the controls it was given to; nothing reads it back.
    #[allow(dead_code)]
    pub panel_font: Option<nwg::Font>,

    pub engine: Option<Engine>,
    pub docs: Vec<Doc>,
    pub current: usize,
    pub switching_tab: bool,
    /// Current shell directory, kept for the status bar and for echoing run commands.
    pub location: String,

    /// Full text shown before the active prompt line. Kept in memory so we never have to
    /// re-read the native control. A WM_SETTEXT write drops the caret; everything inside
    /// here survives between pump ticks without any EM_REPLACESEL calls.
    pub console_buffer: String,

    /// Index just after the prompt: everything before it is scroll-back. Updated whenever
    /// the buffer changes so callers can split history from live input.
    pub prompt_start: i32,
    /// Coloured stretches of `console_buffer`, as UTF-16 offsets. WM_SETTEXT only carries
    /// plain text, so the colours are re-applied to the control after every refresh.
    pub console_runs: Vec<ConsoleRun>,
    pub input_open: bool,
    pub busy: bool,

    /// Modules the background verifier has said will not load in PowerShell 7, with the
    /// reason it gave. The Commands panel hides every command that belongs to one of
    /// these, so a user scanning for `Get-Process` does not have to find out the module
    /// behind it cannot be imported - and the reason is right there in the list.
    pub broken_modules: HashMap<String, String>,

    pub history: Vec<String>,
    pub history_pos: usize,
    /// Text being typed at the current prompt. Kept separate from `console_buffer` so we
    /// can sync the combined document to the native control in one shot.
    pub draft: String,
    /// Current byte-offset of the caret inside `draft`. Set by arrow keys and when a
    /// completion replaces existing typed text; always clamped to [0, draft.len()].
    pub draft_caret: usize,

    /// Inline completion cycle (PSReadLine "choice" style).
    pub candidates: Vec<String>,
    pub candidate_pos: usize,
    pub last_completion: Option<String>,
}

static mut STATE: *mut AppState = std::ptr::null_mut();

pub fn set_state(state: *mut AppState) {
    unsafe {
        STATE = state;
    }
}

/// Raw pointer to the state; callers choose the borrow they need and keep it short.
pub fn state() -> *mut AppState {
    unsafe { STATE }
}

pub fn read<T>(f: impl FnOnce(&AppState) -> T) -> Option<T> {
    let ptr = state();
    if ptr.is_null() {
        return None;
    }
    Some(f(unsafe { &*ptr }))
}

pub fn write<T>(f: impl FnOnce(&mut AppState) -> T) -> Option<T> {
    let ptr = state();
    if ptr.is_null() {
        return None;
    }
    Some(f(unsafe { &mut *ptr }))
}

// ------------------------------------------------------------------ console output

/// Appends coloured text to the internal console buffer without touching the native
/// control. `sync_console` must be called (usually at the end of a pump tick) to push
/// the accumulated buffer into the RichEdit pane in one WM_SETTEXT call.
pub fn console_write(text: &str, fg: [u8; 3]) {
    if text.is_empty() {
        return;
    }
    write(|s| {
        let start = s.console_buffer.encode_utf16().count() as i32;
        s.console_buffer.push_str(text);
        let end = s.console_buffer.encode_utf16().count() as i32;
        if fg == CONSOLE_FG {
            return; // the whole document already has this colour
        }
        // Colours are remembered rather than streamed, because the only write path this
        // control accepts (WM_SETTEXT) replaces the entire document as plain text.
        if let Some(last) = s.console_runs.last_mut() {
            if last.fg == fg && last.end == start {
                last.end = end;
                return;
            }
        }
        s.console_runs.push(ConsoleRun { start, end, fg });
        if s.console_runs.len() > MAX_CONSOLE_RUNS {
            let excess = s.console_runs.len() - MAX_CONSOLE_RUNS;
            s.console_runs.drain(0..excess);
        }
    });
}

/// Forgets the scroll-back and everything coloured inside it.
pub fn console_reset() {
    write(|s| {
        s.console_buffer.clear();
        s.console_runs.clear();
        s.prompt_start = 0;
    });
}

pub fn set_status(text: &str) {
    read(|s| s.status.set_text(0, text));
    if debug_enabled() {
        // The bar is the third place a message goes, and the one a test cannot read: a status
        // control's panels answer `GetWindowText` with nothing. Written here so the sentence the
        // user is shown is in the same log as the request that caused it.
        log_line(&format!("status: {}", text));
    }
}

/// Writes the caret readout, the status bar's second panel. `set_status` owns the first
/// panel, which is where engine and command messages go; the two panels are laid out in
/// `layout`, because native-windows-gui never splits the control.
pub fn set_status_caret(text: &str) {
    read(|s| s.status.set_text(1, text));
}

/// Reads the current length of the internal console buffer.
pub fn console_len() -> i32 {
    read(|s| s.console_buffer.encode_utf16().count() as i32).unwrap_or(-1)
}

/// Set `PS7ISE_DEBUG=1` to get a trace of engine events in `%TEMP%\ps7ise.log`. A GUI
/// build has no console to write to, and this is the cheapest way to see what the
/// console pane was told to show.
pub fn debug_enabled() -> bool {
    std::env::var_os("PS7ISE_DEBUG").is_some()
}

/// Appends a line to `%TEMP%\ps7ise.log`, and does nothing at all unless `PS7ISE_DEBUG` is set.
/// This is a GUI build with no console attached, so a file is the only place start-up problems and
/// the debugger's own traffic can be reported.
///
/// The switch is tested here rather than at each call: the trace is written from a dozen places,
/// and one that forgot to ask would have a release build write to a temporary file on every keystroke.
pub fn log_line(text: &str) {
    use std::io::Write;
    if !debug_enabled() {
        return;
    }
    let path = std::env::temp_dir().join("ps7ise.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{}", text);
    }
}

/// Line and column the caret sits on, counted the way the gutter counts them: the line is
/// the paragraph the control reports, and the column is the character's offset inside that
/// paragraph, so a line wrapped over three rows keeps numbering its characters across all
/// three. It is all asked of the control, so the document never has to be copied.
fn caret_position() -> (i32, i32) {
    use crate::console::{send_message, EM_EXLINEFROMCHAR, EM_LINEFROMCHAR, EM_LINEINDEX};

    let editor = read(|s| s.editor.handle.hwnd().map(|h| h as *mut core::ffi::c_void)).flatten();
    let Some(editor) = editor else {
        return (1, 1);
    };
    let caret = read(|s| s.editor.selection().start).unwrap_or(0) as i32;
    let paragraph = send_message(editor, EM_EXLINEFROMCHAR, 0, caret as isize) as i32 + 1;

    // The column is measured from the paragraph's first character, so walk up over the rows
    // the line was wrapped onto, adding the characters each of them carries.
    let mut row = send_message(editor, EM_LINEFROMCHAR, caret as usize, 0) as i32;
    let mut start = send_message(editor, EM_LINEINDEX, row as usize, 0) as i32;
    let mut col = caret - start;
    while row > 0 {
        let up = send_message(editor, EM_LINEINDEX, (row - 1) as usize, 0) as i32;
        let same_paragraph =
            send_message(editor, EM_EXLINEFROMCHAR, 0, up as isize) as i32 + 1 == paragraph;
        if up >= start || !same_paragraph {
            break;
        }
        col += start - up;
        start = up;
        row -= 1;
    }
    (paragraph.max(1), col.max(0) + 1)
}

/// Repaints the caret readout in the status bar's right-hand panel. Gives back the new text
/// only when the caret actually moved, so the bar is not rewritten twenty five times a
/// second, which the control would show as flicker.
pub fn update_caret_display() -> Option<String> {
    let (line, col) = caret_position();
    let text = format!("Line {}, Col {}", line, col);
    if read(|s| s.caret_text == text).unwrap_or(false) {
        return None;
    }
    write(|s| s.caret_text = text.clone());
    Some(text)
}

/// True when the script pane has scrolled or changed length since the last call, which is
/// when the line-number strip has to be repainted. The gutter stores nothing, so this is
/// the only state that has to be remembered — except the debugger's own generation, which
/// changes when neither the pane nor the strip has moved.
pub fn gutter_changed(key: (i32, i32, i32, u64)) -> bool {
    let changed = read(|s| s.gutter_key != key).unwrap_or(true);
    if changed {
        write(|s| s.gutter_key = key);
    }
    changed
}

/// Gives the console pane the keyboard focus.
pub fn focus_console() {
    read(|s| {
        if let Some(view) = s.console_view.as_ref() {
            view.focus();
        }
    });
}

/// True when `handle` belongs to the control `pick` selects.
pub fn is(
    handle: &nwg::ControlHandle,
    pick: impl FnOnce(&AppState) -> nwg::ControlHandle,
) -> bool {
    read(|s| pick(s) == *handle).unwrap_or(false)
}

/// Puts the caret at the start of a line of the script pane and takes the focus to it, counted
/// the way [`caret_line`] counts them.
///
/// The debug harness calls this through `{LINE:n}`. It exists because the window cannot be
/// driven by posted keystrokes — Windows refuses input to a window that does not own the
/// keyboard — and half of what is worth testing here (F9 on a line, stepping, a breakpoint in
/// the margin) starts from the caret being somewhere particular.
pub fn caret_to_line(line: i32) {
    move_caret(line, true);
}

/// Puts the caret on a line and scrolls it into view *without* taking the focus.
///
/// This is what the debugger does when execution comes to rest: the line it stopped on may be
/// forty rows below what the pane is showing, and an arrow the user cannot see is a stop they
/// cannot see. The focus stays where it was, because the expression box is where the text the
/// user is typing lives, and a stop at the next breakpoint while a question is half written
/// would put those characters into the script.
pub fn reveal_line(line: i32) {
    move_caret(line, false);
}

fn move_caret(line: i32, focus: bool) {
    let offset = read(|s| {
        // The control ends its lines with \r; the count below works in \n.
        let text = crate::console::normalize_breaks(&s.editor.text());
        let mut offset = 0usize;
        let mut found = 0i32;
        for name in text.split_inclusive('\n') {
            if found == line - 1 {
                return Some(offset);
            }
            // The control counts UTF-16 code units, and a line break costs one whether the
            // text spells it \n or \r\n.
            offset += name.encode_utf16().count();
            found += 1;
        }
        // Past the last line: the end of the text, which is where the caret goes rather than
        // nowhere, because a test that asks for line 40 of a five line script should still see
        // the caret move.
        Some(text.encode_utf16().count())
    })
    .flatten();

    let Some(offset) = offset else {
        if debug_enabled() {
            // No text to count: the editor is empty, or there is no document. A caret that does
            // not move looks from outside like a token that was never received, and this says
            // which of the two it was.
            log_line(&format!("caret: line {} refused, the script pane holds nothing", line));
        }
        return;
    };
    if debug_enabled() {
        log_line(&format!("caret: line {} at offset {}", line, offset));
    }
    write(|s| {
        if focus {
            s.editor.focus();
        }
        s.editor.set_selection(offset as u32..offset as u32);
        // EM_SCROLLCARET: `set_selection` moves the caret but does not promise to scroll to it,
        // so a caret placed off screen stays off screen. The debugger is the caller that cares —
        // it stops wherever the script happens to be.
        if let Some(hwnd) = s.editor.handle.hwnd() {
            crate::console::send_message(
                hwnd as *mut std::ffi::c_void,
                crate::console::EM_SCROLLCARET,
                0,
                0,
            );
        }
    });
    update_caret_display();
}

/// The command a menu item or toolbar button stands for.
pub fn command_for(handle: &nwg::ControlHandle) -> Option<cmd::Id> {
    read(|s| {
        s.items
            .iter()
            .chain(s.button_items.iter())
            .find(|(_, item)| item == handle)
            .map(|(id, _)| *id)
    })
    .flatten()
}

fn set_title(text: &str) {
    read(|s| {
        if let Some(hwnd) = s.window.handle.hwnd() {
            crate::console::set_window_text(hwnd as *mut core::ffi::c_void, text);
        }
    });
}

// ------------------------------------------------------------------ engine pump

/// While `PS7ISE_DEBUG=1` is set, whatever is written to `%TEMP%\ps7ise-inject.txt` is replayed
/// into the console pane as if it had been typed, and the file is taken away. Windows refuses to
/// deliver keystrokes that another process posts to a window without the keyboard focus, so this is
/// the only way an automated test can drive the prompt.
fn drain_input_file() {
    if !debug_enabled() {
        return;
    }
    let path = std::env::temp_dir().join("ps7ise-inject.txt");
    // An empty file is skipped rather than taken. A writer that has just created the file holds it
    // open with nothing in it yet, and taking it away from under that writer means the bytes it
    // then writes go into a file this pump has already deleted - which is a keystroke that vanishes
    // without trace, the single worst thing a test harness can do to itself.
    if std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0) == 0 {
        return;
    }
    // The script is *renamed* rather than read and then emptied. Reading first leaves a window
    // between the read and the truncate: a test that writes its next key inside that window has it
    // wiped by the truncate, and a lost keystroke is the worst kind of test failure, because the
    // program looks like it ignored a key rather than like it never saw one. A rename hands the
    // bytes over in one step — after it the file is either fully this pump's or fully still there.
    let taken = path.with_extension("taken");
    if std::fs::rename(&path, &taken).is_err() {
        // Nothing to read, or a writer still holding it open; the next tick will find it.
        return;
    }
    let script = std::fs::read_to_string(&taken).unwrap_or_default();
    if script.is_empty() {
        // Belt and braces: whatever it was that left the file empty, give it back rather than
        // throw it away, unless the writer has already made a new one.
        if path.exists() {
            let _ = std::fs::remove_file(&taken);
        } else {
            let _ = std::fs::rename(&taken, &path);
        }
        return;
    }
    let _ = std::fs::remove_file(&taken);
    // What was replayed, written first: when a test complains that a key did nothing, the question
    // is whether the window ignored it or never received it, and the log answers it.
    log_line(&format!("inject: {}", script.replace('\n', " ")));
    crate::input::inject(&script);
}

/// Called on every timer tick: moves engine output into the console and keeps the
/// input line in step with what the shell is doing.
pub fn pump_engine() {
    drain_input_file();
    let events = write(|s| match s.engine.as_mut() {
        Some(engine) => engine.drain_events(96),
        None => Vec::new(),
    })
    .unwrap_or_default();

    if !events.is_empty() {
        for event in &events {
            apply_event(event);
        }
        sync_console();
        if debug_enabled() {
            // The kinds are what tells a phantom prompt apart from a real one: a prompt the
            // shell did not mean to print arrives next to something that grew the input line.
            let kinds: Vec<String> = events
                .iter()
                .map(|event| format!("{:?}({})", event.kind, event.text.chars().count()))
                .collect();
            log_line(&format!(
                "pump: {} [{}] buffer {} prompt_start {}",
                events.len(),
                kinds.join(" "),
                console_len(),
                read(|s| s.prompt_start).unwrap_or(-1)
            ));
        }
    }

    // The Commands column reads its answers here too: they arrive on the engine's reader
    // thread, and a list of four thousand names cannot be applied to a window from there.
    crate::panel::pump();

    // The same for the debugger: a stop is an event that has to be seen even when the shell has
    // moved on by the time the window next looks, and the answers about it arrive in pieces
    // between the console's own output.
    crate::debug_pane::pump();

    let ended = read(|s| s.engine.as_ref().map(|e| e.is_ended()).unwrap_or(false))
        .unwrap_or(false);
    if ended {
        log_line(&format!(
            "engine ended, exit code {}",
            read(|s| s.engine.as_ref().map(|e| e.exit_code()).unwrap_or(0))
                .unwrap_or(-1)
        ));
        set_status("PowerShell session ended");
        write(|s| s.input_open = false);
        nwg::stop_thread_dispatch();
    }

    // Safety net: text that arrives with line breaks inside (a multi-line paste) is fed
    // to the shell one line at a time, exactly as a console would.
    let stray = read(|s| {
        if !s.input_open {
            return false;
        }
        normalize_breaks(&text_from_buffer(s.prompt_start, &s.console_buffer, &s.draft)).contains('\n')
    })
    .unwrap_or(false);
    if stray {
        crate::input::submit_input();
    }
}

/// Reassembles the live input line visible to the user: scroll-back + current draft.
pub fn text_from_buffer(prompt_start: i32, buffer: &str, draft: &str) -> String {
    let buf_end = buffer.encode_utf16().count() as i32;
    if prompt_start < buf_end {
        // There is still some scroll-back after the prompt marker — treat it as part of
        // the visible input region for the stray-line safety net.
        normalize_breaks(&buffer[prompt_start as usize..].to_string()) + draft
    } else {
        draft.to_string()
    }
}

/// Pushes the full internal console document into the RichEdit control once per tick.
/// WM_SETTEXT is the only write this control accepts here, so the document is replaced
/// wholesale and then re-styled: base font and colour first, then the coloured runs the
/// engine reported, then the caret back at the end of the input line.
pub fn sync_console() {
    // Snapshot what we need before acquiring the write lock.
    let (buffer, draft, runs) = read(|s| {
        (
            s.console_buffer.clone(),
            s.draft.clone(),
            s.console_runs.clone(),
        )
    })
    .unwrap_or_default();

    let Some(hwnd_raw) = read(|s| s.console_view.as_ref().map(|v| v.hwnd())) else {
        return;
    };
    let hwnd = hwnd_raw.unwrap_or(std::ptr::null_mut());
    if hwnd.is_null() {
        return;
    }

    let combined = format!("{}{}", buffer, draft);
    let encoded_len = combined.encode_utf16().count() as i32;
    let view = crate::console::ConsoleView::new(hwnd);
    view.set_text(&combined);
    view.apply_style(CONSOLE_FG, CONSOLE_FONT, CONSOLE_HEIGHT);
    for run in &runs {
        view.color_range(run.start, run.end, run.fg);
    }
    view.select(encoded_len, encoded_len);
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 1);
        UpdateWindow(hwnd);
    }
}

fn apply_event(event: &crate::engine::PsEvent) {
    match event.kind {
        EventKind::Output | EventKind::InputEcho => console_write(&event.text, event.fg),
        // A progress record is written for a console that can move the cursor back and draw
        // the bar over itself. This control has no such thing: a carriage return is a hard
        // line break, so the bar would leave a line per update and the "bar finished" record
        // - `\r`, a hundred and twenty spaces, `\r` - would leave two blank lines. Worse, that
        // text lands on the input line, where the pump reads a line break in it and hands an
        // empty command to the shell (see the stray-line safety net below): reading the
        // Commands column, which finishes a pipeline of its own, was repeating the prompt.
        EventKind::Progress => {
            let text = progress_text(&event.text);
            if !text.is_empty() {
                console_write(&text, event.fg);
            }
        }
        EventKind::Error => console_write(&event.text, ERROR_FG),
        EventKind::Warning => console_write(&event.text, event.fg),
        EventKind::Verbose | EventKind::Debug => console_write(&event.text, event.fg),
        EventKind::Title => {
            if !event.text.trim().is_empty() {
                set_title(&format!(
                    "{} - PowerShell 7 ISE v{}",
                    event.text.trim(),
                    env!("CARGO_PKG_VERSION")
                ));
            }
        }
        EventKind::CommandStarted => {
            write(|s| {
                s.busy = true;
                s.candidates.clear();
                s.candidate_pos = 0;
            });
            set_status("Running... (Ctrl+C or the Stop button to interrupt)");
        }
        EventKind::CommandFinished => {
            write(|s| s.busy = false);
            trim_console();
            set_status("Ready");
        }
        EventKind::InputRequested => {
            let location = event.text.trim().to_string();
            write(|s| {
                s.busy = false;
                s.input_open = true;
                s.history_pos = s.history.len();
                s.draft.clear();
                s.candidates.clear();
                s.candidate_pos = 0;
                s.last_completion = None;
                if !location.is_empty() {
                    s.location = location.clone();
                }
                s.prompt_start = s.console_buffer.encode_utf16().count() as i32;
            });
            focus_console();
            if !location.is_empty() {
                set_status(&location);
            }
        }
        EventKind::Unknown => console_write(&event.text, CONSOLE_FG),
        EventKind::Verification => {
            // `name|reason` on the wire: the verifier emits one verdict per module, and the
            // panel hides the module's commands because of it. Nothing is written to the
            // console: this is a background scan that runs while the user is looking at the
            // command list, and a wall of red text about modules they never asked about is
            // exactly the kind of noise a background task should not produce.
            let text = event.text.trim();
            let (name, reason) = match text.split_once('|') {
                Some((n, r)) => (n.trim(), r.trim()),
                None => (text, ""),
            };
            if !name.is_empty() {
                write(|s| {
                    s.broken_modules.insert(name.to_string(), reason.to_string());
                });
                if crate::state::debug_enabled() {
                    crate::state::log_line(&format!(
                        "verification: the {} module could not be loaded: {}",
                        name, reason
                    ));
                }
            }
        }
    }
}

/// A progress record as the console pane can show it: the carriage return the engine puts at
/// the front to redraw the bar in place is dropped, and the record that only says the bar is
/// finished - `\r`, spaces, `\r` - gives back nothing, because there is nothing in it worth
/// keeping and a line of spaces would look like the shell had finished talking.
fn progress_text(raw: &str) -> String {
    let trimmed = raw.trim_matches(|c| c == '\r' || c == ' ');
    if trimmed.is_empty() {
        return String::new();
    }
    format!("{}\n", trimmed)
}

fn trim_console() {
    write(|s| {
        let total = s.console_buffer.encode_utf16().count() as i32;
        if total > MAX_CONSOLE_CHARS {
            let keep = KEEP_CONSOLE_CHARS as usize;
            let byte_limit = char_byte_len(&s.console_buffer, keep);
            let dropped = (total - keep as i32).max(0);
            s.console_buffer.truncate(byte_limit);
            s.prompt_start -= dropped;
            // The coloured runs are offsets into the buffer that just moved.
            for run in s.console_runs.iter_mut() {
                run.start -= dropped;
                run.end -= dropped;
            }
            s.console_runs.retain(|run| run.end > 0);
            if let Some(first) = s.console_runs.first_mut() {
                first.start = first.start.max(0);
            }
        }
    });
}

/// Byte length of the first `chars` UTF-16 code units of `text`.
fn char_byte_len(text: &str, chars: usize) -> usize {
    let mut count = 0usize;
    let mut bytes = 0usize;
    for c in text.chars() {
        bytes += c.len_utf8();
        count += 1;
        if count >= chars {
            break;
        }
    }
    bytes
}

// ------------------------------------------------------------------ asking the debugger

/// Asks the shell one debugger question, and waits for nothing: an answer arrives in pieces
/// over the following ticks, so the most the caller can be told is that the question was put.
pub fn debug_request(verb: &str, argument: &str) {
    if debug_enabled() {
        log_line(&format!("debug: {} {}", verb, argument));
    }
    write(|s| {
        if let Some(engine) = s.engine.as_mut() {
            engine.debug_request(verb, argument);
        }
    });
}

/// Asks the shell to go on: continue, step into, step over, step out, or give up on the script.
/// Nothing is cleared here, because the engine is the one that says a stop has ended, and a
/// window that guessed when it ended would be showing a frame it invented.
pub fn debug_resume(verb: &str) {
    debug_request(verb, "");
}

/// Puts a breakpoint on a line of the script being edited, or takes the one that is there off.
/// This is what F9 means, and what a click in the gutter means.
pub fn toggle_breakpoint(line: i32) {
    let Some(path) = current_script_path() else {
        // The one case this cannot work, said plainly: the shell keeps a line breakpoint
        // against a file on disk, so a tab that has never been saved has nowhere to put one.
        set_status("Save the script before setting a breakpoint: a breakpoint belongs to a file.");
        beep();
        return;
    };

    match read(|s| s.debug.breakpoint_at(&path, line).map(|point| point.id)).flatten() {
        Some(id) => debug_request("b", &format!("-{}", id)),
        None => debug_request("b", &format!("+{} {}", line, path)),
    }
}

/// Takes every breakpoint off — in this script and in every other one the shell still carries,
/// because that is what the shell's own "remove all" means.
pub fn clear_breakpoints() {
    debug_request("b", "c");
}

/// Asks for the breakpoint list. Sent once the shell is up, and after anything that can change
/// it, because the gutter is drawn from the answer rather than from what the window asked for.
pub fn list_breakpoints() {
    debug_request("l", "");
}

/// The breakpoints this script carries, as line and armed state, in one call: the gutter draws
/// every visible row and would otherwise ask the question, and resolve the script's path, once
/// per row per repaint.
pub fn script_breakpoints() -> Vec<(i32, bool)> {
    let Some(path) = current_script_path() else {
        return Vec::new();
    };
    read(|s| {
        s.debug
            .breakpoints
            .iter()
            .filter(|point| point.script.eq_ignore_ascii_case(&path))
            .map(|point| (point.line, point.enabled))
            .collect()
    })
    .unwrap_or_default()
}

/// The line execution is held on, if it is held on a line of the script being edited. A stop in
/// some other file has no mark to draw in this gutter, and putting one on the same line number
/// of an unrelated script would send the user to the wrong place.
pub fn stop_line() -> Option<i32> {
    let path = current_script_path()?;
    let stop = read(|s| s.debug.stop.clone())??;
    if stop.script.is_empty() || !stop.script.eq_ignore_ascii_case(&path) {
        return None;
    }
    Some(stop.line)
}

// ------------------------------------------------------------------ the debugger

/// The line the caret is on, counted the way the gutter counts them, so a breakpoint asked for
/// from the keyboard lands on the line the user is looking at.
pub fn caret_line() -> i32 {
    caret_position().0
}

/// The script the pane is showing, in the form a breakpoint is attached to: a full path with no
/// `\\?\` on the front of it, that prefix being how Windows spells "do not shorten this" where
/// PowerShell reports a script name without it.
///
/// Gives back nothing for a tab that has never been saved. A PowerShell line breakpoint belongs
/// to a file rather than to a buffer, so an unsaved script has nowhere to put one - and a
/// window that drew a mark for a breakpoint the shell turned down would be a window whose
/// gutter cannot be believed.
pub fn current_script_path() -> Option<String> {
    let path = read(|s| s.docs.get(s.current)?.path.clone())??;
    let full = std::path::absolute(&path).ok()?;
    let text = full.to_string_lossy().to_string();
    Some(text.strip_prefix(r"\\?\").unwrap_or(&text).to_string())
}

/// True while execution is held at a stop. This is the engine's own answer rather than a guess
/// made here from the last command the window sent: the shell can stop and resume faster than
/// the window is painted, and a menu keyed to what the window *asked* for would be greyed out
/// wrongly, at the one moment the user needs it.
pub fn debug_stopped() -> bool {
    read(|s| s.engine.as_ref().map(|engine| engine.is_stopped()).unwrap_or(false))
        .unwrap_or(false)
}
