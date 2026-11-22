//! Line numbers down the left-hand side of the script pane.
//!
//! A rich edit control has no gutter, so the strip beside it is a window of this module's
//! own window class: [`wndproc`] answers its `WM_PAINT` by reading the rows out of the
//! editor, and [`refresh`] asks for a repaint when the pane scrolls or its text changes.
//!
//! The window is deliberately not an `nwg::Label`. That control installs a `WM_NCCALCSIZE`
//! hook which shrinks its client area down to the height of its text so it can centre the
//! text vertically, which would report a strip of eleven pixels for a pane three hundred
//! rows tall and clip every number but the first.
//!
//! Numbers are attached to display rows by asking the editor which paragraph each visible
//! row starts with, which keeps the numbering right when a long line is wrapped over
//! several rows: the continuation rows are left blank, the way every editor does it.

use crate::console::{
    send_message, EM_EXLINEFROMCHAR, EM_GETFIRSTVISIBLELINE, EM_GETLINECOUNT, EM_LINEINDEX,
    EM_POSFROMCHAR,
};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Registered once, by [`create`]; the strip is the only window of this class.
const CLASS_NAME: &str = "PS7ISEGutter";

// ------------------------------------------------------------------ win32 declarations

const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_GETFONT: u32 = 0x003D;
const WM_SETFONT: u32 = 0x0030;
const WM_GETDLGCODE: u32 = 0x0087;
const WM_LBUTTONDOWN: u32 = 0x0201;
const WM_MOUSEWHEEL: u32 = 0x020A;
const WM_SETCURSOR: u32 = 0x0020;
const WM_PRINTCLIENT: u32 = 0x0318;
/// `HTCLIENT`, the only part of this window a cursor question can be about.
const HTCLIENT: isize = 1;

const WS_CHILD: u32 = 0x4000_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const SW_HIDE: i32 = 0;
const SW_SHOW: i32 = 5;
/// `MAKEINTRESOURCEW(IDI_APPLICATION)`, the arrow. A plain margin does not ask for a caret.
const IDC_ARROW: *const u16 = 32512usize as *const u16;
/// `MAKEINTRESOURCEW(IDI_HAND)`, the pointing hand: the margin asks for a breakpoint.
const IDC_HAND: *const u16 = 32649usize as *const u16;
/// `NULL_PEN`, so a mark is its colour and nothing else. A stroked circle would sit inside a
/// black ring, which on a dark margin is a dark ring and on a light one a hard edge.
const NULL_PEN: i32 = -3;

const DT_RIGHT: u32 = 0x0002;
const DT_VCENTER: u32 = 0x0004;
const DT_SINGLELINE: u32 = 0x0020;
const DT_NOPREFIX: u32 = 0x0800;
const TRANSPARENT: i32 = 1;

/// The strip is the same colour as the pane beside it, the numbers are the grey of a margin note,
/// and a hairline separates the two. None of them is a constant any more: a white strip next to a
/// dark pane, or a dark strip next to a white one, is the half-dark window `theme` exists to
/// prevent. COLORREF is 0x00BBGGRR, the reverse of the order the palette keeps colours in.
fn gutter_bg() -> u32 {
    crate::console::colorref(crate::theme::palette().gutter_bg) as u32
}

fn number_fg() -> u32 {
    crate::console::colorref(crate::theme::palette().gutter_fg) as u32
}

fn separator() -> u32 {
    crate::console::colorref(crate::theme::palette().gutter_sep) as u32
}

/// The three marks a debugging session puts in the margin. They come from the palette because
/// a dark red dot on a dark margin is a margin with a missing line number on it.
fn breakpoint_fg(enabled: bool) -> u32 {
    let palette = crate::theme::palette();
    crate::console::colorref(if enabled {
        palette.breakpoint
    } else {
        palette.breakpoint_off
    }) as u32
}

fn stop_fg() -> u32 {
    crate::console::colorref(crate::theme::palette().stop_fg) as u32
}

fn stop_bg() -> u32 {
    crate::console::colorref(crate::theme::palette().stop_bg) as u32
}

/// Row height to fall back on when the pane holds a single row, so there is no second row
/// to measure against. Only used for rows that do not exist in that case.
const FALLBACK_ROW_H: i32 = 18;

#[repr(C)]
#[derive(Clone, Copy)]
struct PointL {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// Matches the win32 `WNDCLASSEXW`. Field for field, in the order the header gives them:
/// `cbSize` has to come out as `size_of` of this struct or the registration is refused.
#[repr(C)]
struct WndClassEx {
    cb_size: u32,
    style: u32,
    lpfn_wnd_proc: *mut c_void,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: *mut c_void,
    h_icon: *mut c_void,
    h_cursor: *mut c_void,
    hbr_background: *mut c_void,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: *mut c_void,
}

/// Matches the win32 `PAINTSTRUCT`. `BeginPaint` fills it in and validates the update
/// region, which is what stops the strip from being asked to paint again straight away.
#[repr(C)]
struct PaintStruct {
    hdc: *mut c_void,
    erase: i32,
    paint_rc: Rect,
    restore: i32,
    inc_update: i32,
    reserved: [u8; 32],
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassExW(class: *const WndClassEx) -> u16;
    fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: *mut c_void,
        menu: *mut c_void,
        instance: *mut c_void,
        param: *mut c_void,
    ) -> *mut c_void;
    fn DefWindowProcW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn MoveWindow(hwnd: *mut c_void, x: i32, y: i32, width: i32, height: i32, repaint: i32) -> i32;
    fn ShowWindow(hwnd: *mut c_void, command: i32) -> i32;
    fn LoadCursorW(instance: *mut c_void, name: *const u16) -> *mut c_void;
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn GetWindowRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
    fn ClientToScreen(hwnd: *mut c_void, point: *mut PointL) -> i32;
    fn BeginPaint(hwnd: *mut c_void, ps: *mut PaintStruct) -> *mut c_void;
    fn EndPaint(hwnd: *mut c_void, ps: *const PaintStruct) -> i32;
    fn DrawTextW(hdc: *mut c_void, text: *const u16, count: i32, rect: *mut Rect, flags: u32)
        -> i32;
    fn FillRect(hdc: *mut c_void, rect: *const Rect, brush: *mut c_void) -> i32;
    fn CreateSolidBrush(color: u32) -> *mut c_void;
}

#[link(name = "gdi32")]
extern "system" {
    fn SelectObject(hdc: *mut c_void, obj: *mut c_void) -> *mut c_void;
    fn SetTextColor(hdc: *mut c_void, color: u32) -> u32;
    fn SetBkMode(hdc: *mut c_void, mode: i32) -> i32;
    fn DeleteObject(obj: *mut c_void) -> i32;
    fn Ellipse(
        hdc: *mut c_void,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    ) -> i32;
    fn Polygon(hdc: *mut c_void, points: *const PointL, count: i32) -> i32;
    fn GetStockObject(object: i32) -> *mut c_void;
}

#[link(name = "user32")]
extern "system" {
    fn SetCursor(cursor: *mut c_void) -> *mut c_void;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
}

/// A control's text as a win32 wide string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// ------------------------------------------------------------------ the window

static REGISTERED: AtomicBool = AtomicBool::new(false);

/// Bumped whenever the debugger changes what the margin shows, so [`refresh`] notices a change
/// that has nothing to do with scrolling.
static MARKS: AtomicU64 = AtomicU64::new(0);

/// Registers the gutter class the first time it is asked for. The class has no background
/// brush because [`paint`] covers the whole client area itself.
fn register_class() -> bool {
    if REGISTERED.load(Ordering::SeqCst) {
        return true;
    }

    let name = wide(CLASS_NAME);
    // The cursor has to be a handle rather than `MAKEINTRESOURCE(IDC_ARROW)`: with a real
    // `hInstance` below, Windows reads an ordinal as a resource *in this exe*, does not find
    // one, and refuses the registration with ERROR_INVALID_PARAMETER.
    let cursor = unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) };
    let class = WndClassEx {
        cb_size: std::mem::size_of::<WndClassEx>() as u32,
        style: 0,
        lpfn_wnd_proc: wndproc as *const () as *mut c_void,
        cb_cls_extra: 0,
        cb_wnd_extra: 0,
        h_instance: unsafe { GetModuleHandleW(std::ptr::null()) },
        h_icon: std::ptr::null_mut(),
        h_cursor: cursor,
        hbr_background: std::ptr::null_mut(),
        lpsz_menu_name: std::ptr::null(),
        lpsz_class_name: name.as_ptr(),
        h_icon_sm: std::ptr::null_mut(),
    };

    let atom = unsafe { RegisterClassExW(&class) };
    // The error has to be taken before anything else runs: logging reads an environment
    // variable, and that overwrites it with success.
    let error = unsafe { GetLastError() };
    if atom == 0 {
        if crate::state::debug_enabled() {
            crate::state::log_line(&format!(
                "gutter: RegisterClassExW failed, size {} error {}",
                std::mem::size_of::<WndClassEx>(),
                error
            ));
        }
        return false;
    }
    REGISTERED.store(true, Ordering::SeqCst);
    true
}

/// Creates the strip as a child of `parent` and gives it the font its numbers are drawn
/// with. The handle is never destroyed: like the rest of the interface it goes away with
/// the window it belongs to.
pub fn create(parent: *mut c_void, font: *mut c_void) -> Option<*mut c_void> {
    if !register_class() {
        return None;
    }

    let name = wide(CLASS_NAME);
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            name.as_ptr(),
            std::ptr::null(),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            1,
            1,
            parent,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    if hwnd.is_null() {
        if crate::state::debug_enabled() {
            crate::state::log_line(&format!(
                "gutter: CreateWindowExW failed, error {}",
                unsafe { GetLastError() }
            ));
        }
        return None;
    }

    send_message(hwnd, WM_SETFONT, font as usize, 1);
    Some(hwnd)
}

/// Moves and sizes the strip. [`layout`](crate::layout) calls it beside the editor, whose
/// window the strip is laid out against.
pub fn set_geometry(strip: *mut c_void, x: i32, y: i32, width: i32, height: i32) {
    unsafe { MoveWindow(strip, x, y, width.max(1), height.max(1), 1) };
}

/// Follows the script pane when it is shown or hidden.
pub fn set_visible(strip: *mut c_void, visible: bool) {
    unsafe { ShowWindow(strip, if visible { SW_SHOW } else { SW_HIDE }) };
}

extern "system" fn wndproc(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { message(hwnd, msg, wparam, lparam) }
}

unsafe fn message(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize {
    match msg {
        WM_PAINT => {
            let mut ps: PaintStruct = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !hdc.is_null() {
                paint(hdc, hwnd);
                EndPaint(hwnd, &ps);
            }
            0
        }
        // Everything the strip shows is drawn by `paint`, in one pass over the client area,
        // so the parent's brush is refused: it would sit over the numbers for a moment on
        // every scroll, which reads as flicker.
        WM_ERASEBKGND => 1,
        // `PrintWindow` asks a child for its client area through this message rather than
        // `WM_PAINT`, so answering it is what keeps the numbers in a screenshot.
        WM_PRINTCLIENT => {
            let hdc = wparam as *mut c_void;
            if !hdc.is_null() {
                paint(hdc, hwnd);
            }
            0
        }
        // The strip is a margin, but a wheel over it should still move the script. The
        // coordinates of a wheel message are screen coordinates, so the pane can take the
        // message as it stands.
        WM_MOUSEWHEEL => match editor() {
            Some(editor) => {
                send_message(editor, msg, wparam, lparam);
                0
            }
            None => DefWindowProcW(hwnd, msg, wparam, lparam),
        },
        // The strip takes no keyboard input; the script pane is the only thing that does.
        WM_GETDLGCODE => 0,

        // A click in the margin sets or clears the breakpoint on that line, which is what the
        // margin is for in every editor that has one. The coordinates are inside the strip, and
        // the strip is as tall as the pane, so one row of the strip is one row of the script.
        WM_LBUTTONDOWN => {
            let y = ((lparam as i32) >> 16) as i16 as i32;
            if let Some(editor) = editor() {
                if let Some(line) = line_at_y(hwnd, editor, y) {
                    crate::state::toggle_breakpoint(line);
                }
            }
            0
        }

        // The whole strip answers to a click, so the whole strip asks for the pointing hand. A
        // cursor that changed only over the eight pixels of an existing dot would tell the user
        // where the breakpoints are but not that the margin takes clicks in the first place.
        WM_SETCURSOR => {
            // Only for the client area: Windows also asks this when the cursor is over a border
            // of the window this strip belongs to, and answering there would put a hand on the
            // resize edge.
            let hit = (lparam as i32 & 0xFFFF) as u16 as isize;
            let hand = LoadCursorW(std::ptr::null_mut(), IDC_HAND);
            if hit == HTCLIENT && !hand.is_null() {
                SetCursor(hand);
                return 1;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn editor() -> Option<*mut c_void> {
    crate::state::read(|s| s.editor.handle.hwnd().map(|h| h as *mut c_void)).flatten()
}

// ------------------------------------------------------------------ painting

/// Pixel y of the top of the row that starts with display row `row`, measured inside the
/// editor so it survives scrolling, wrapping and the control's own padding. The row height
/// is a by-product: the next row down is exactly one row further.
fn row_top(editor: *mut c_void, row: i32, row_h: &mut i32) -> Option<i32> {
    let ch = line_index(editor, row)?;
    let top = char_point(editor, ch);
    match line_index(editor, row + 1).map(|next| char_point(editor, next)) {
        Some(y) if y > top => *row_h = y - top,
        _ if *row_h <= 0 => *row_h = FALLBACK_ROW_H,
        _ => {}
    }
    Some(top)
}

fn line_index(editor: *mut c_void, row: i32) -> Option<i32> {
    // The message answers with a character index, or -1 past the last row. Every rich edit
    // answer fits in an i32, being a row or a character offset.
    let ch = send_message(editor, EM_LINEINDEX, row as usize, 0) as i32;
    if ch < 0 {
        None
    } else {
        Some(ch)
    }
}

fn char_point(editor: *mut c_void, ch: i32) -> i32 {
    let mut pt = PointL { x: 0, y: 0 };
    send_message(
        editor,
        EM_POSFROMCHAR,
        ch as usize,
        &mut pt as *mut _ as isize,
    );
    pt.y
}

/// The document line (1-based) that a display row starts with.
fn line_at(editor: *mut c_void, row: i32) -> Option<i32> {
    let ch = line_index(editor, row)?;
    Some(send_message(editor, EM_EXLINEFROMCHAR, 0, ch as isize) as i32 + 1)
}

/// How many rows the pane holds.
fn line_count(editor: *mut c_void) -> i32 {
    (send_message(editor, EM_GETLINECOUNT, 0, 0) as i32).max(1)
}

/// Draws the numbers into `hdc`, which is either the strip's own paint device or the one a
/// `WM_PRINTCLIENT` caller supplied. `strip` is only asked for its size and its font.
unsafe fn paint(hdc: *mut c_void, strip: *mut c_void) {
    let Some(editor) = editor() else { return };

    let (wide, high) = crate::console::client_size(strip);
    let rc = Rect {
        left: 0,
        top: 0,
        right: wide,
        bottom: high,
    };
    fill(hdc, &rc, gutter_bg());
    let edge = Rect {
        left: rc.right - 1,
        top: 0,
        right: rc.right,
        bottom: rc.bottom,
    };
    fill(hdc, &edge, separator());

    // The strip is laid out against the editor's *window*, while the positions the editor
    // reports are inside its client area. Mapping the one origin into screen space and
    // comparing it with the other is the border that sits above the first row.
    let mut wr = Rect::default();
    GetWindowRect(editor, &mut wr);
    let mut origin = PointL { x: 0, y: 0 };
    ClientToScreen(editor, &mut origin);
    let border = (origin.y - wr.top).max(0);

    // The strip's own font, which is smaller than the script's: the numbers are placed by
    // the rows of the pane, not by the lines of this text, so a size of its own is what
    // makes the gutter read as a margin rather than as a column of code.
    let font = send_message(strip, WM_GETFONT, 0, 0) as *mut c_void;
    let old_font = if font.is_null() {
        std::ptr::null_mut()
    } else {
        SelectObject(hdc, font)
    };
    SetTextColor(hdc, number_fg());
    SetBkMode(hdc, TRANSPARENT);

    let mut row_h = 0i32;
    let first = (send_message(editor, EM_GETFIRSTVISIBLELINE, 0, 0) as i32).max(0);
    let mut y = match row_top(editor, first, &mut row_h) {
        Some(top) => top + border,
        None => {
            if !old_font.is_null() {
                SelectObject(hdc, old_font);
            }
            return;
        }
    };

    // What the margin is showing beside the numbers: every breakpoint this script carries, and
    // the line execution is held on. Both are asked for once per repaint rather than once per
    // row, because a repaint covers forty rows and the answers do not change while it runs.
    let marks = crate::state::script_breakpoints();
    let stop = crate::state::stop_line();

    let mut shown = -1i32;
    let mut row = 0i32;
    while y < rc.bottom {
        let line = match line_at(editor, first + row) {
            Some(line) => line,
            None => break,
        };
        // A wrapped line carries its number on its first row only; the rows it runs into
        // are left empty, which is what makes them read as the same line. The marks go on
        // that same first row, so a line wrapped over three rows has one dot, not three.
        if line != shown {
            // The row of the line execution is held on is banded before anything is drawn on
            // it, so the margin reads as one highlighted line rather than as a number that
            // happens to be a different colour.
            if Some(line) == stop {
                let band = Rect {
                    left: 0,
                    top: y,
                    right: rc.right - 1,
                    bottom: y + row_h,
                };
                fill(hdc, &band, stop_bg());
            }
            let armed = marks.iter().find(|mark| mark.0 == line).map(|mark| mark.1);
            if let Some(armed) = armed {
                dot(hdc, y, row_h, armed);
            }
            if Some(line) == stop {
                // The line execution stopped on is very often the line the breakpoint is on, and
                // the two marks live in the same column. The arrow goes beside the dot rather
                // than over it, because a stop that hides its own breakpoint tells the user the
                // breakpoint is gone at the exact moment they most want to see it.
                arrow(hdc, y, row_h, if armed.is_some() { 11 } else { 2 });
            }

            let mut box_rc = Rect {
                left: 0,
                top: y,
                right: rc.right - 6,
                bottom: y + row_h,
            };
            let text: Vec<u16> = line
                .to_string()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            // The line execution is stopped on is the one number in the margin the user is
            // looking for, so it is not drawn in margin grey.
            SetTextColor(hdc, if Some(line) == stop { stop_fg() } else { number_fg() });
            DrawTextW(
                hdc,
                text.as_ptr(),
                -1,
                &mut box_rc,
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            shown = line;
        }
        row += 1;
        y += row_h;
        if row > line_count(editor) {
            break; // more rows than the pane holds: the arithmetic has gone wrong
        }
    }

    if !old_font.is_null() {
        SelectObject(hdc, old_font);
    }
}

/// The dot of a line breakpoint, in the column at the left of the numbers. A disabled
/// breakpoint gets the same dot in a colour the eye reads as off: an empty margin would say
/// there is nothing there, and there is something there that has merely been switched off.
unsafe fn dot(hdc: *mut c_void, top: i32, row_h: i32, enabled: bool) {
    let diameter = 7.min(row_h.max(4) - 1);
    let left = 3;
    let above = top + (row_h - diameter) / 2;
    let brush = CreateSolidBrush(breakpoint_fg(enabled));
    let pen = GetStockObject(NULL_PEN);
    let old_brush = SelectObject(hdc, brush);
    let old_pen = SelectObject(hdc, pen);
    Ellipse(hdc, left, above, left + diameter, above + diameter);
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    if !brush.is_null() {
        DeleteObject(brush);
    }
}

/// The arrow on the line execution is held on. It points at the text rather than sitting in
/// front of it, which is the difference between a mark and a character the user has to delete.
///
/// `left` is where it starts: the margin's mark column, except on the line that carries a
/// breakpoint too, where the dot is drawn first and the arrow has to stand beside it.
unsafe fn arrow(hdc: *mut c_void, top: i32, row_h: i32, left: i32) {
    let high = 7.min(row_h.max(5));
    let middle = top + row_h / 2;
    let points = [
        PointL { x: left, y: middle - high / 2 },
        PointL { x: left, y: middle + high / 2 },
        PointL {
            x: left + high,
            y: middle,
        },
    ];
    let brush = CreateSolidBrush(stop_fg());
    let pen = GetStockObject(NULL_PEN);
    let old_brush = SelectObject(hdc, brush);
    let old_pen = SelectObject(hdc, pen);
    Polygon(hdc, points.as_ptr(), points.len() as i32);
    SelectObject(hdc, old_brush);
    SelectObject(hdc, old_pen);
    if !brush.is_null() {
        DeleteObject(brush);
    }
}

/// The document line whose first row is under `y`, a point inside the strip. The walk is the
/// one [`paint`] makes, because a click has to land on the line the number beside it names —
/// and the two can only agree by being measured the same way.
unsafe fn line_at_y(strip: *mut c_void, editor: *mut c_void, y: i32) -> Option<i32> {
    let (_, high) = crate::console::client_size(strip);
    if y < 0 || y >= high {
        return None;
    }

    // The same offset from the pane's window to its text that `paint` has to allow for, or the
    // first row of the script would answer to clicks above the margin.
    let mut wr = Rect::default();
    GetWindowRect(editor, &mut wr);
    let mut origin = PointL { x: 0, y: 0 };
    ClientToScreen(editor, &mut origin);
    let border = (origin.y - wr.top).max(0);

    let mut row_h = 0i32;
    let first = (send_message(editor, EM_GETFIRSTVISIBLELINE, 0, 0) as i32).max(0);
    let top = row_top(editor, first, &mut row_h)? + border;
    if y < top || row_h <= 0 {
        // Above the first row of text there is margin and nothing else.
        return None;
    }

        // The document line the click is on. `line_at` maps a row to the line that row's first
        // character belongs to, which already covers the rows a wrapped line ran into.
        let mut row = (y - top) / row_h;
        if row > line_count(editor) {
            row = line_count(editor) - 1;
        }
        line_at(editor, first + row.max(0))
    }

fn fill(hdc: *mut c_void, rect: &Rect, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        if !brush.is_null() {
            FillRect(hdc, rect, brush);
            DeleteObject(brush);
        }
    }
}

// ------------------------------------------------------------------ repainting

/// What the strip has to be repainted for: the top row of the pane, how many rows it holds,
/// how tall the strip itself got, and how many times the debugger has changed what the margin
/// shows. `refresh` compares this with the last one it saw, so the update timer does not
/// invalidate the gutter twenty five times a second — and so that a breakpoint set or a stop
/// reached repaints it once, without the timer having to notice the difference on its own.
fn visible_key(strip: *mut c_void, editor: *mut c_void) -> (i32, i32, i32, u64) {
    let (_, height) = crate::console::client_size(strip);
    (
        send_message(editor, EM_GETFIRSTVISIBLELINE, 0, 0) as i32,
        line_count(editor),
        height,
        MARKS.load(Ordering::SeqCst),
    )
}

/// Repaints the strip when the pane has scrolled, changed length, or been resized. Three
/// messages and a comparison, so the update timer can call it every tick.
pub fn refresh(strip: *mut c_void, editor: *mut c_void) {
    let key = visible_key(strip, editor);
    if crate::state::gutter_changed(key) {
        // The strip paints all of itself, so Windows is not asked to erase it first.
        unsafe { crate::state::InvalidateRect(strip, std::ptr::null(), 0) };
    }
}

/// Asks for the margin to be drawn again, because what it draws has changed underneath it: a
/// breakpoint was added or taken away, or execution came to rest on a line. The scroll position
/// has not moved, so nothing else would notice.
pub fn invalidate() {
    MARKS.fetch_add(1, Ordering::SeqCst);
    if let Some(strip) = crate::state::read(|s| s.line_gutter).flatten() {
        unsafe { crate::state::InvalidateRect(strip, std::ptr::null(), 0) };
    }
}
