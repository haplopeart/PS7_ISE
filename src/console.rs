//! Low-level RichEdit helpers that turn a rich text control into a console pane.
//!
//! Everything here talks to the window handle directly. Colour/format is applied
//! through `nwg::RichTextBox::set_char_format` by the caller, because native-windows-gui
//! already marshals CHARFORMAT correctly.

use std::ffi::c_void;

const WM_USER: u32 = 0x0400;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_CHAR: u32 = 0x0102;

const EM_EXSETSEL: u32 = WM_USER + 55;
const EM_EXGETSEL: u32 = WM_USER + 52;
const EM_SETBKGNDCOLOR: u32 = WM_USER + 67;

/// Row and paragraph arithmetic. These are taken from `winuser.h` and `richedit.h` in the
/// Windows SDK rather than from memory: the numbering is neither sequential nor guessable,
/// and a wrong number is simply answered with zero. The line-number gutter counts with the
/// same messages, so the status bar readout and the margin cannot disagree.
/// How many display rows the control holds.
pub const EM_GETLINECOUNT: u32 = 0x00BA;
/// Character index that starts a given display row.
pub const EM_LINEINDEX: u32 = 0x00BB;
/// Display row a character sits on.
pub const EM_LINEFROMCHAR: u32 = 0x00C9;
/// First visible row of the control. Counts display rows, so it knows about wrapping.
pub const EM_GETFIRSTVISIBLELINE: u32 = 0x00CE;
/// Pixel position of a character, in the control's client area.
pub const EM_POSFROMCHAR: u32 = WM_USER + 38;
/// Paragraph a character belongs to: the number the user counts as "line 1, line 2...".
pub const EM_EXLINEFROMCHAR: u32 = WM_USER + 54;

/// commctrl: replaces the panels of a status bar.
const SB_SETPARTS: u32 = WM_USER + 4;

/// A status bar is one window that draws its own panels, and native-windows-gui never
/// splits it, so `SB_SETPARTS` is sent here: without it there is a single panel and the
/// caret readout has nowhere to go. `widths` are the right edges of the panels, and a last
/// entry of -1 means "whatever is left".
pub fn set_status_parts(hwnd: *mut c_void, widths: &[i32]) {
    unsafe {
        SendMessageW(
            hwnd,
            SB_SETPARTS,
            widths.len(),
            widths.as_ptr() as isize,
        )
    };
}

pub const VK_CONTROL: i32 = 0x11;
pub const VK_SHIFT: i32 = 0x10;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct CharRange {
    pub start: i32,
    pub end: i32,
}

#[link(name = "user32")]
extern "system" {
    pub fn SendMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
    pub fn GetKeyState(virtual_key: i32) -> i16;
    fn GetWindowTextLengthW(hwnd: *mut c_void) -> i32;
    fn GetWindowTextW(hwnd: *mut c_void, buffer: *mut u16, max_count: i32) -> i32;
    fn GetClientRect(hwnd: *mut c_void, rect: *mut i32) -> i32;
    fn SetFocus(hwnd: *mut c_void) -> *mut c_void;
    fn SetWindowTextW(hwnd: *mut c_void, text: *const u16) -> i32;
}

/// Sets a window's text with WM_SETTEXT; native-windows-gui only lets the builder do it.
/// For a control this replaces its whole buffer, for a window it is the title bar.
pub fn set_window_text(hwnd: *mut c_void, text: &str) {
    let mut buffer: Vec<u16> = text.encode_utf16().collect();
    buffer.push(0);
    unsafe { SetWindowTextW(hwnd, buffer.as_ptr()) };
}

/// `RGB()` as Windows stores it: 0x00BBGGRR.
pub fn colorref(rgb: [u8; 3]) -> usize {
    (rgb[0] as usize) | ((rgb[1] as usize) << 8) | ((rgb[2] as usize) << 16)
}

/// Sends a message the caller has to name itself, for the controls native-windows-gui
/// does not wrap: the tab strip's header messages, and the rich edit's Cut/Copy/Paste.
pub fn send_message(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { SendMessageW(hwnd, msg, wparam, lparam) }
}

pub const WM_CUT: u32 = 0x0300;
pub const WM_COPY: u32 = 0x0301;
pub const WM_PASTE: u32 = 0x0302;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_CONTEXTMENU: u32 = 0x007B;

/// Size of a window's client area, in pixels.
pub fn client_size(hwnd: *mut c_void) -> (i32, i32) {
    let mut rect = [0i32; 4];
    unsafe { GetClientRect(hwnd, rect.as_mut_ptr()) };
    (rect[2] - rect[0], rect[3] - rect[1])
}

pub fn ctrl_pressed() -> bool {
    unsafe { GetKeyState(VK_CONTROL) < 0 }
}

pub fn shift_pressed() -> bool {
    unsafe { GetKeyState(VK_SHIFT) < 0 }
}

/// A console pane: a rich text control plus the caret/selection discipline a shell needs.
pub struct ConsoleView {
    hwnd: *mut c_void,
}

impl ConsoleView {
    /// Wraps an existing rich text window handle.
    pub fn new(hwnd: *mut c_void) -> ConsoleView {
        ConsoleView { hwnd }
    }

    pub fn hwnd(&self) -> *mut c_void {
        self.hwnd
    }

    /// Number of characters currently in the control.
    pub fn len(&self) -> i32 {
        unsafe { GetWindowTextLengthW(self.hwnd) }
    }

    pub fn focus(&self) {
        unsafe {
            SetFocus(self.hwnd);
        }
    }

    pub fn set_background(&self, rgb: [u8; 3]) {
        unsafe {
            SendMessageW(self.hwnd, EM_SETBKGNDCOLOR, 0, colorref(rgb) as isize);
        }
    }

    /// Selects a half-open character range; start == end positions the caret.
    pub fn select(&self, start: i32, end: i32) {
        let range = CharRange { start, end };
        unsafe {
            SendMessageW(
                self.hwnd,
                EM_EXSETSEL,
                0,
                &range as *const CharRange as isize,
            );
        }
    }

    pub fn selection(&self) -> CharRange {
        let mut range = CharRange::default();
        unsafe {
            SendMessageW(
                self.hwnd,
                EM_EXGETSEL,
                0,
                &mut range as *mut CharRange as isize,
            );
        }
        range
    }

    /// All text from `start` to the end of the buffer.
    pub fn text_from(&self, start: i32) -> String {
        let total = self.len();
        if start >= total {
            return String::new();
        }
        let text = self.text();
        let units: Vec<u16> = text.encode_utf16().collect();
        let from = start.max(0).min(total) as usize;
        if from >= units.len() {
            return String::new();
        }
        String::from_utf16_lossy(&units[from..])
    }

    /// The whole buffer. Rich text controls keep this cheap enough for occasional reads.
    pub fn text(&self) -> String {
        let total = self.len() as usize;
        if total == 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; total + 2];
        let written =
            unsafe { GetWindowTextW(self.hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        let end = (written.max(0) as usize).min(total);
        String::from_utf16_lossy(&buffer[..end])
    }

    /// Replaces the entire buffer with WM_SETTEXT, which the control always accepts.
    pub fn set_text(&self, text: &str) {
        set_window_text(self.hwnd, text);
    }

    /// Applies the console look — `face` at `height` twips, drawn in `fg` — to the whole
    /// document and to anything inserted later. WM_SETTEXT throws away all character
    /// formatting, so this has to run after every full-document refresh.
    pub fn apply_style(&self, fg: [u8; 3], face: &str, height: i32) {
        let cf = CharFormat::styled(fg, face, height);
        let ptr = &cf as *const CharFormat as isize;
        unsafe {
            SendMessageW(self.hwnd, EM_SETCHARFORMAT, SCF_ALL, ptr);
            SendMessageW(self.hwnd, EM_SETCHARFORMAT, SCF_DEFAULT, ptr);
        }
    }

    /// Colours one half-open range of the document, used to replay the colours the engine
    /// reported once the plain text is in place.
    pub fn color_range(&self, start: i32, end: i32, fg: [u8; 3]) {
        if end <= start {
            return;
        }
        self.select(start, end);
        let cf = CharFormat::styled(fg, "", 0);
        unsafe {
            SendMessageW(
                self.hwnd,
                EM_SETCHARFORMAT,
                SCF_SELECTION,
                &cf as *const CharFormat as isize,
            );
        }
    }
}

const EM_SETCHARFORMAT: u32 = WM_USER + 68;
const SCF_SELECTION: usize = 0x1;
const SCF_ALL: usize = 0x4;
const SCF_DEFAULT: usize = 0x0001_0000;
/// Only the members flagged in the mask are read by the control.
const CFM_COLOR: u32 = 0x4000_0000;
const CFM_FACE: u32 = 0x2000_0000;
const CFM_SIZE: u32 = 0x8000_0000;
const DEFAULT_CHARSET: u8 = 1;

/// `CHARFORMATW` from richedit.h, laid out exactly as the SDK declares it. The control
/// compares `cb_size` against its own `sizeof`, so a structure that is even one field off
/// makes every EM_SETCHARFORMAT fail without any visible symptom.
#[repr(C)]
#[derive(Clone, Copy)]
struct CharFormat {
    cb_size: u32,
    dw_mask: u32,
    dw_effects: u32,
    y_height: i32,
    y_offset: i32,
    clr_text: u32,
    b_char_set: u8,
    b_pitch_and_family: u8,
    sz_face_name: [u16; 32],
    w_weight: u16,
    s_spacing: i16,
    cr_back_color: u32,
    lcid: u32,
    reserved: u32,
    s_style: i16,
    w_kerning: u16,
    b_underline_type: u8,
    b_animation: u8,
    b_rev_author: u8,
    b_underline_color: u8,
}

impl CharFormat {
    /// A format setting colour, plus the face and size when `face` is not empty. `height`
    /// is the character height in twips (points * 20). COLORREF is 0x00BBGGRR.
    fn styled(rgb: [u8; 3], face: &str, height: i32) -> CharFormat {
        let mut name = [0u16; 32];
        for (slot, unit) in name.iter_mut().zip(face.encode_utf16()) {
            *slot = unit;
        }
        let mut mask = CFM_COLOR;
        if !face.is_empty() {
            mask |= CFM_FACE | CFM_SIZE;
        }
        CharFormat {
            cb_size: std::mem::size_of::<CharFormat>() as u32,
            dw_mask: mask,
            dw_effects: 0,
            y_height: height,
            y_offset: 0,
            clr_text: (rgb[0] as u32) | ((rgb[1] as u32) << 8) | ((rgb[2] as u32) << 16),
            b_char_set: DEFAULT_CHARSET,
            b_pitch_and_family: 0,
            sz_face_name: name,
            w_weight: 0,
            s_spacing: 0,
            cr_back_color: 0,
            lcid: 0,
            reserved: 0,
            s_style: 0,
            w_kerning: 0,
            b_underline_type: 0,
            b_animation: 0,
            b_rev_author: 0,
            b_underline_color: 0,
        }
    }
}

/// Normalizes the paragraph marks a rich text control reports so the shell only has to
/// deal with one kind of line break.
pub fn normalize_breaks(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}
