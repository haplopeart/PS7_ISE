//! Mutable application state plus everything the console pane can be asked to do.
//!
//! native-windows-gui hands control access to `'static` closures, so the state lives in
//! one heap allocation reached through a pointer. Every access happens on the UI thread.

use crate::console::{normalize_breaks, ConsoleView};
use crate::engine::{Engine, EventKind};
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

    pub const HELP: u32 = 40;
    pub const ABOUT: u32 = 41;
    /// Relaunch the program with Administrator privileges, like the shield icon on the
    /// original ISE's Run button.
    pub const RUN_AS_ADMIN: u32 = 42;
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
    pub gutter_key: (i32, i32, i32),

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

/// Appends a line to `%TEMP%\ps7ise.log`. This is a GUI build with no console attached,
/// so a file is the only place start-up problems can be reported.
pub fn log_line(text: &str) {
    use std::io::Write;
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
/// the only state that has to be remembered.
pub fn gutter_changed(key: (i32, i32, i32)) -> bool {
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

/// While `PS7ISE_DEBUG=1` is set, whatever is appended to `%TEMP%\ps7ise-inject.txt` is
/// replayed into the console pane as if it had been typed, and the file is emptied. Windows
/// refuses to deliver keystrokes that another process posts to a window without the
/// keyboard focus, so this is the only way an automated test can drive the prompt.
fn drain_input_file() {
    if !debug_enabled() {
        return;
    }
    let path = std::env::temp_dir().join("ps7ise-inject.txt");
    let Ok(script) = std::fs::read_to_string(&path) else {
        return;
    };
    if script.is_empty() {
        return;
    }
    let _ = std::fs::write(&path, "");
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
            log_line(&format!(
                "pump: applied {} events, buffer len {} prompt_start {}",
                events.len(),
                console_len(),
                read(|s| s.prompt_start).unwrap_or(-1)
            ));
        }
    }

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
        EventKind::Output | EventKind::Progress | EventKind::InputEcho => {
            console_write(&event.text, event.fg)
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
    }
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
