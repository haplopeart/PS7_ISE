//! Keyboard behaviour of the console pane: input, history, completion, interrupt,
//! and running the script pane.
//!
//! All live text lives inside `AppState.draft`. The control itself only receives a
//! full WM_SETTEXT refresh once per pump tick via [`state::sync_console`].

use crate::state::{beep, console_write, read, write};

pub const CONSOLE_FG: [u8; 3] = [0xCC, 0xCC, 0xCC];
pub const DIM_FG: [u8; 3] = [0x9B, 0x9B, 0x9B];

fn is_boundary(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | '|' | ',' | '(' | ')' | '{' | '}' | '&' | '=' | '@')
}

/// Byte offset inside `s` after advancing `n` UTF-16 code points.
fn utf16_byte_len(s: &str, n: usize) -> usize {
    let mut bytes = 0usize;
    for c in s.chars().take(n) {
        bytes += c.len_utf8();
    }
    bytes
}

/// Byte offset inside `s` after consuming UTF-16 units up to (but not including) `byte_target`.
fn byte_to_utf16_index(s: &str, byte_target: usize) -> usize {
    if byte_target == 0 { return 0; }
    let mut bytes = 0usize;
    let mut chars = 0usize;
    for c in s.chars() {
        if bytes >= byte_target { break; }
        bytes += c.len_utf8();
        chars += 1;
    }
    chars
}

/// The line currently being typed plus the caret column inside it.
fn current_line() -> (i32, String, usize) {
    read(|s| {
        let prompt_start = s.prompt_start;
        let first_line: String = s.draft.lines().next().unwrap_or("").to_string();
        let col = s.draft_caret.min(first_line.chars().count());
        (prompt_start, first_line, col)
    })
    .unwrap_or((0, String::new(), 0))
}

/// Sets the draft to `text` and moves the caret to its end.
fn replace_input(text: &str) {
    let chars = text.chars().count();
    write(|s| {
        s.draft = text.to_string();
        s.draft_caret = chars;
    });
    crate::state::sync_console();
}

/// Replaces the UTF-16 code-point range `[start, end)` inside the draft with `text`,
/// moving the caret to after the inserted text.
fn replace_range(start: usize, end: usize, text: &str) {
    write(|s| {
        let d = &s.draft;
        let u16_total = d.encode_utf16().count();
        let u16_start = start.min(u16_total);
        let u16_end = end.min(u16_total);
        let byte_start = utf16_byte_len(d, u16_start);
        let byte_end = utf16_byte_len(d, u16_end);
        let before = &d[..byte_start];
        let after = &d[byte_end..];
        s.draft = format!("{}{}{}", before, text, after);
        s.draft_caret = u16_start + text.chars().count();
    });
    crate::state::sync_console();
}

/// Locks the input line and queues each non-empty line for the shell.
fn hand_to_shell(lines: &[String]) {
    write(|s| {
        s.input_open = false;
        s.candidates.clear();
        s.candidate_pos = 0;
        s.last_completion = None;
        s.draft.clear();
        s.draft_caret = 0;
    });
    crate::state::sync_console();
    for line in lines {
        write(|s| {
            if !line.trim().is_empty() && s.history.last().map(|last| last != line).unwrap_or(true) {
                s.history.push(line.clone());
            }
            s.history_pos = s.history.len();
        });
        write(|s| {
            if let Some(engine) = s.engine.as_mut() {
                engine.execute(line);
            }
        });
    }
}

pub fn submit_input() {
    if !read(|s| s.input_open).unwrap_or(false) {
        beep();
        return;
    }
    let raw = read(|s| s.draft.clone()).unwrap_or_default();

    let mut lines: Vec<String> = raw.split('\n').filter(|l| !l.is_empty()).map(str::to_string).collect();
    if lines.is_empty() {
        lines.push(String::new());
    }

    // The Enter key belongs to the shell, so the control never receives its line break.
    console_write("\n", CONSOLE_FG);
    hand_to_shell(&lines);
}

pub fn recall_history(up: bool) {
    if !read(|s| s.input_open).unwrap_or(false) {
        beep();
        return;
    }
    let _ = current_line();
    let index = write(|s| {
        if s.draft.is_empty() {
            s.draft = s
                .console_view
                .as_ref()
                .map(|v| v.text_from(s.prompt_start))
                .unwrap_or_default();
        }
        if up {
            if s.history_pos > 0 { s.history_pos -= 1; }
        } else if s.history_pos < s.history.len() {
            s.history_pos += 1;
        }
        s.history_pos
    })
    .unwrap_or(0);

    let total = read(|s| s.history.len()).unwrap_or(0);
    let text = if index >= total {
        read(|s| s.draft.clone()).unwrap_or_default()
    } else {
        read(|s| s.history.get(index).cloned().unwrap_or_default()).unwrap_or_default()
    };
    let text = text.lines().next().unwrap_or("").to_string();
    replace_input(&text);
}

pub fn clear_line() {
    replace_input("");
}

pub fn stop_execution() {
    write(|s| {
        if let Some(engine) = s.engine.as_mut() {
            engine.stop();
        }
    });
    console_write("^C\n", DIM_FG);
}

fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else { return String::new(); };
    let chars: Vec<char> = first.chars().collect();
    let mut len = chars.len();
    for item in items.iter().skip(1) {
        let other: Vec<char> = item.chars().collect();
        let mut i = 0;
        while i < len && i < other.len() && chars[i] == other[i] { i += 1; }
        len = i;
    }
    chars[..len].iter().collect()
}

/// The token the caret sits in, as (absolute control offset start, absolute end, text).
fn token_at_caret() -> (i32, i32, String) {
    read(|s| {
        let prompt_start = s.prompt_start;
        let draft = &s.draft;
        let byte_off = utf16_byte_len(draft, s.draft_caret);
        let u16_at = byte_to_utf16_index(draft, byte_off);
        let units: Vec<u16> = draft.encode_utf16().collect();
        let at = u16_at.min(units.len());

        let boundary = |c: u16| -> bool {
            is_boundary(char::from_u32(c as u32).unwrap_or(' '))
        };

        let mut start = at;
        while start > 0 && !boundary(units[start - 1]) { start -= 1; }
        let mut end = at;
        while end < units.len() && !boundary(units[end]) { end += 1; }
        let text = String::from_utf16_lossy(&units[start..end]);
        (prompt_start + start as i32, prompt_start + end as i32, text)
    })
    .unwrap_or((0, 0, String::new()))
}

/// Tab: completes the token under the caret. Repeating Tab cycles through the matches.
pub fn complete() {
    if !read(|s| s.input_open).unwrap_or(false) {
        beep();
        return;
    }

    let previous = read(|s| s.last_completion.clone()).unwrap_or_default();
    let candidates = read(|s| s.candidates.clone()).unwrap_or_default();
    let reuse = previous.as_deref() == Some(token_at_caret().2.as_str()) && !candidates.is_empty();
    if reuse { cycle_candidate(); return; }

    let buffer = read(|s| s.draft.clone()).unwrap_or_default();
    let buffer = buffer.lines().next().unwrap_or("").to_string();
    let (_tok_abs_start, tok_abs_end, token) = token_at_caret();
    let buf_len = read(|s| s.console_buffer.encode_utf16().count() as i32).unwrap_or(0);
    let caret_col = (tok_abs_end - buf_len).max(0) as usize;

    let items = write(|s| match s.engine.as_mut() {
        Some(engine) => engine.completions(&buffer, caret_col),
        None => Vec::new(),
    }).unwrap_or_default();

    if items.is_empty() { beep(); return; }

    let prefix = common_prefix(&items);
    let (insert, pos) = if prefix.len() > token.len() {
        (prefix, usize::MAX)
    } else {
        (items[0].clone(), 0)
    };

    let (tok_abs_start, tok_abs_end, _) = token_at_caret();
    let u16_start = (tok_abs_start - buf_len).max(0) as usize;
    let u16_end = (tok_abs_end - buf_len).max(0) as usize;
    replace_range(u16_start, u16_end, &insert);
    write(|s| {
        s.candidates = items;
        s.candidate_pos = pos;
        s.last_completion = Some(token);
    });
}

fn cycle_candidate() {
    let (items, next) = write(|s| {
        if s.candidates.is_empty() { return (Vec::new(), 0); }
        s.candidate_pos = (s.candidate_pos + 1) % s.candidates.len();
        (s.candidates.clone(), s.candidate_pos)
    }).unwrap_or((Vec::new(), 0));

    let Some(text) = items.get(next) else { return; };
    let (start, end, _) = token_at_caret();
    let buf_len = read(|s| s.console_buffer.encode_utf16().count() as i32).unwrap_or(0);
    let u16_start = (start - buf_len).max(0) as usize;
    let u16_end = (end - buf_len).max(0) as usize;
    replace_range(u16_start, u16_end, text);
}

pub fn control_c() {
    let has_selection = read(|s| {
        s.console_view.as_ref().map(|view| {
            let sel = view.selection();
            sel.start != sel.end
        }).unwrap_or(false)
    }).unwrap_or(false);

    if has_selection {
        copy_selection(); return;
    }

    if read(|s| s.busy).unwrap_or(false) {
        stop_execution();
    } else {
        clear_line();
        console_write("^C", DIM_FG);
    }
}

fn copy_selection() {
    let text = read(|s| {
        let Some(view) = s.console_view.as_ref() else { return String::new(); };
        let sel = view.selection();
        let (lo, hi) = (sel.start.min(sel.end), sel.start.max(sel.end));
        let all = view.text();
        let units: Vec<u16> = all.encode_utf16().collect();
        let a = lo.max(0).min(units.len() as i32) as usize;
        let b = hi.max(0).min(units.len() as i32) as usize;
        String::from_utf16_lossy(&units[a..b.max(a)])
    }).unwrap_or_default();

    if text.is_empty() { return; }
    write_clipboard(&text);
}

fn write_clipboard(text: &str) {
    use std::ffi::c_void;
    #[link(name = "user32")] extern "system" {
        fn OpenClipboard(hwnd: *mut c_void) -> i32;
        fn EmptyClipboard() -> i32;
        fn CloseClipboard() -> i32;
        fn SetClipboardData(format: u32, data: *mut c_void) -> *mut c_void;
    }
    #[link(name = "kernel32")] extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(handle: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(handle: *mut c_void) -> i32;
    }
    const CF_UNICODE_TEXT: u32 = 13;
    const GMEM_MOVEABLE: u32 = 0x2;
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = units.len() * 2;
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 { return; }
        EmptyClipboard();
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if !handle.is_null() {
            let dst = GlobalLock(handle);
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(units.as_ptr() as *const u8, dst as *mut u8, bytes);
                GlobalUnlock(handle);
                SetClipboardData(CF_UNICODE_TEXT, handle);
            }
        }
        CloseClipboard();
    }
}

pub fn clear_console() {
    crate::state::console_reset();
    write(|s| {
        s.draft.clear();
        s.draft_caret = 0;
    });
    crate::state::sync_console();
    // The prompt was part of the text that just went away, so have the shell print it
    // again rather than leaving an empty pane to type into.
    write(|s| {
        if let Some(engine) = s.engine.as_mut() {
            engine.redraw_prompt();
        }
    });
}

// ------------------------------------------------------------------ keystrokes

fn insert_typed(text: &str) {
    let before = text.chars().count();
    write(|s| {
        let caret = s.draft_caret.min(s.draft.chars().count());
        s.draft.insert_str(caret, text);
        s.draft_caret = caret + before;
    });
    crate::state::sync_console();
}

fn caret_pos() -> i32 {
    read(|s| s.prompt_start + s.draft_caret as i32).unwrap_or(0)
}

pub fn on_char(ch: char) -> bool {
    if !read(|s| s.input_open).unwrap_or(false) { return true; }

    match ch {
        '\r' | '\n' => { submit_input(); true }
        '\t' => { complete(); true }
        '\u{1b}' => { clear_line(); true }
        '\u{3}' => { control_c(); true }
        '\u{8}' => {
            let buf_len = read(|s| s.console_buffer.encode_utf16().count() as i32).unwrap_or(0);
            if caret_pos() > buf_len {
                write(|s| {
                    if s.draft_caret > 0 {
                        let idx = s.draft_caret - 1;
                        let byte_idx = utf16_byte_len(&s.draft, idx);
                        // String::remove takes a byte index and drops the whole Unicode
                        // scalar value that starts there.
                        s.draft.remove(byte_idx);
                        s.draft_caret -= 1;
                    } else {
                        beep();
                    }
                });
                crate::state::sync_console();
                true
            } else {
                beep();
                true
            }
        }
        '\u{7f}' => { delete_word_back(); true }
        c if c.is_control() => {
            match c { 'v' => paste(), 'l' => clear_console(), _ => {} }
            true
        }
        c => { insert_typed(&c.to_string()); true }
    }
}

fn delete_word_back() {
    let buf_len = read(|s| s.console_buffer.encode_utf16().count() as i32).unwrap_or(0);
    if caret_pos() <= buf_len { beep(); return; }
    let draft_caret = read(|s| s.draft_caret).unwrap_or(0);
    if draft_caret == 0 { return; }
    let draft = read(|s| s.draft.clone()).unwrap_or_default();
    let byte_off = utf16_byte_len(&draft, draft_caret);
    // Walk back past whitespace, then past word chars.
    let mut at = byte_off;
    while at > 0 {
        let prev = draft.chars().nth(at.saturating_sub(1)).unwrap_or(' ');
        if !is_boundary_char(prev) { break; }
        at -= 1;
    }
    while at > 0 {
        let prev = draft.chars().nth(at.saturating_sub(1)).unwrap_or(' ');
        if is_boundary_char(prev) { break; }
        at -= 1;
    }
    let new_caret = byte_to_utf16_index(&draft, at);
    write(|s| {
        s.draft.drain(at..byte_off);
        s.draft_caret = new_caret;
    });
    crate::state::sync_console();
}

fn is_boundary_char(c: char) -> bool {
    is_boundary(c)
}

pub fn on_keydown(vk: u32) -> bool {
    use crate::console::{ctrl_pressed, shift_pressed};

    if ctrl_pressed() {
        match vk {
            VK_C => { control_c(); return true; }
            VK_V => { paste(); return true; }
            VK_A => {
                let max = read(|s| s.draft.chars().count()).unwrap_or(0);
                write(|s| { s.draft_caret = max; });
                crate::state::sync_console();
                return true;
            }
            _ => return false,
        }
    }

    if !read(|s| s.input_open).unwrap_or(false) {
        return matches!(vk, VK_PRIOR | VK_NEXT | VK_UP | VK_DOWN);
    }

    match vk {
        VK_UP | VK_DOWN => {
            recall_history(vk == VK_UP);
            true
        }
        VK_LEFT => {
            if shift_pressed() { false }
            else {
                write(|s| {
                    if s.draft_caret > 0 {
                        s.draft_caret -= 1;
                    } else {
                        beep();
                    }
                });
                crate::state::sync_console();
                true
            }
        }
        VK_RIGHT => {
            let max = read(|s| s.draft.chars().count()).unwrap_or(0);
            write(|s| {
                if s.draft_caret < max {
                    s.draft_caret += 1;
                }
            });
            crate::state::sync_console();
            true
        }
        VK_HOME => {
            write(|s| { s.draft_caret = 0; });
            crate::state::sync_console();
            true
        }
        VK_END => {
            let max = read(|s| s.draft.chars().count()).unwrap_or(0);
            write(|s| { s.draft_caret = max; });
            crate::state::sync_console();
            true
        }
        VK_DELETE => {
            let max = read(|s| s.draft.chars().count()).unwrap_or(0);
            write(|s| {
                if s.draft_caret >= max {
                    beep();
                } else {
                    let idx = s.draft_caret;
                    s.draft.remove(idx);
                }
            });
            crate::state::sync_console();
            true
        }
        VK_PRIOR | VK_NEXT => false,
        _ => false,
    }
}

pub fn paste() {
    if !read(|s| s.input_open).unwrap_or(false) { return; }
    let text = read_clipboard();
    if text.is_empty() { return; }
    let text = text.replace('\r', "");
    if !text.contains('\n') {
        insert_typed(&text); return;
    }
    let mut lines: Vec<String> = text.split('\n').map(|l| l.to_string()).collect();
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) { lines.pop(); }
    let (Some(last), rest) = (lines.pop(), lines) else { return; };
    for line in rest {
        insert_typed(&line);
        submit_input();
    }
    insert_typed(&last);
}

fn read_clipboard() -> String {
    use std::ffi::c_void;
    #[link(name = "user32")] extern "system" {
        fn OpenClipboard(hwnd: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(format: u32) -> *mut c_void;
    }
    #[link(name = "kernel32")] extern "system" {
        fn GlobalLock(handle: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(handle: *mut c_void) -> i32;
    }
    const CF_UNICODE_TEXT: u32 = 13;
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 { return String::new(); }
        let mut result = String::new();
        if IsClipboardFormatAvailable(CF_UNICODE_TEXT) != 0 {
            let handle = GetClipboardData(CF_UNICODE_TEXT);
            if !handle.is_null() {
                let ptr = GlobalLock(handle) as *const u16;
                if !ptr.is_null() {
                    let mut len = 0usize;
                    while *ptr.add(len) != 0 { len += 1; }
                    let units = std::slice::from_raw_parts(ptr, len);
                    result = String::from_utf16_lossy(units);
                    GlobalUnlock(handle);
                }
            }
        }
        CloseClipboard();
        result
    }
}

pub const VK_C: u32 = 0x43;
pub const VK_V: u32 = 0x56;
pub const VK_A: u32 = 0x41;
pub const VK_UP: u32 = 0x26;
pub const VK_DOWN: u32 = 0x28;
pub const VK_LEFT: u32 = 0x25;
pub const VK_RIGHT: u32 = 0x27;
pub const VK_HOME: u32 = 0x24;
pub const VK_END: u32 = 0x23;
pub const VK_PRIOR: u32 = 0x21;
pub const VK_NEXT: u32 = 0x22;
pub const VK_DELETE: u32 = 0x2E;

// ------------------------------------------------------------------ scripted input

/// Feeds scripted text through the same handlers the keyboard uses.
///
/// Windows refuses input that another process posts to a window which does not own the
/// keyboard focus, so the console pane cannot be driven from a test in any other way.
/// `state::drain_input_file` calls this with the contents of `%TEMP%\ps7ise-inject.txt`,
/// and only while `PS7ISE_DEBUG=1` is set.
///
/// Plain characters go through [`on_char`]. `{NAME}` tokens stand for the keys that arrive
/// as WM_KEYDOWN rather than as a character, so `{UP}`, `{TAB}`, `{CTRL_C}` and friends
/// behave exactly as if they had been pressed. A brace that does not open a known token is
/// typed literally, which keeps PowerShell hashtables like `@{a=1}` intact.
pub fn inject(script: &str) {
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '{' {
            if let Some(len) = chars[i + 1..].iter().position(|&c| c == '}') {
                let name: String = chars[i + 1..i + 1 + len].iter().collect();
                if inject_key(&name) {
                    i += len + 2;
                    continue;
                }
            }
        }
        on_char(chars[i]);
        i += 1;
    }
    crate::state::sync_console();
}

/// Types the keystroke a `{NAME}` token stands for. Returns false for unknown names.
fn inject_key(name: &str) -> bool {
    let vk = match name.to_ascii_uppercase().as_str() {
        "TAB" => return on_char('\t'),
        "ENTER" => {
            submit_input();
            return true;
        }
        "ESC" => return on_char('\u{1b}'),
        "BS" => return on_char('\u{8}'),
        "CTRL_C" => {
            control_c();
            return true;
        }
        "CTRL_V" => {
            paste();
            return true;
        }
        "UP" => VK_UP,
        "DOWN" => VK_DOWN,
        "LEFT" => VK_LEFT,
        "RIGHT" => VK_RIGHT,
        "HOME" => VK_HOME,
        "END" => VK_END,
        "PGUP" => VK_PRIOR,
        "PGDN" => VK_NEXT,
        "DEL" => VK_DELETE,
        _ => return false,
    };
    on_keydown(vk)
}
