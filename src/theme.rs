//! The palette: the colours the window is drawn in, chosen to match the operating system.
//!
//! Windows 10 added a per-user "choose your default app mode" setting, stored as
//! `AppsUseLightTheme` under the Themes\Personalize key. What it does *not* do is change the GDI
//! system colours: measured on a machine in dark mode, `GetSysColor(COLOR_WINDOW)` still answers
//! `#C8C8C8` and `COLOR_WINDOWTEXT` still answers black. So a program that colours its controls
//! from `GetSysColor` gets a light window over a dark shell, which is the half-dark result this
//! module exists to avoid. The palette below is therefore explicit, and the registry value is the
//! only thing read from the system.
//!
//! Two other things are needed, and neither is a colour:
//!
//! * The common controls - the buttons, the tab strip, the status bar, the scroll bars - are drawn
//!   by comctl32 from the visual style, and they ignore everything in this file. They are told to
//!   go dark through an unpublished `uxtheme.dll` entry point, called by ordinal because no header
//!   declares it. It fails quietly on builds that do not have it, and the chrome stays light: an
//!   ugly window, not a broken one.
//! * The title bar and its buttons belong to the desktop window manager, which has a documented
//!   attribute for exactly this (`DWMWA_USE_IMMERSIVE_DARK_MODE`, numbered 20 from Windows 10 2004
//!   and 19 before that).
//!
//! The console pane keeps its dark background in both modes. The colours PowerShell reports for
//! text are the sixteen `ConsoleColor` values, rendered by the engine through the Campbell palette
//! it ships with, and that palette assumes a dark background: `Gray` is `#CCCCCC`, which is the
//! normal text colour and which vanishes on white. A console that changed with the theme would
//! need the engine's palette inverted to match, so the console stays a terminal and the rest of
//! the window follows the user's setting.

use crate::console::colorref;
use std::ffi::c_void;
use std::sync::OnceLock;

#[link(name = "advapi32")]
extern "system" {
    fn RegGetValueW(
        key: *const c_void,
        sub_key: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut u8,
        length: *mut u32,
    ) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn GetClientRect(hwnd: *mut c_void, rect: *mut i32) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateSolidBrush(color: u32) -> *mut c_void;
    fn SelectObject(dc: *mut c_void, obj: *mut c_void) -> *mut c_void;
    fn SetTextColor(dc: *mut c_void, color: u32) -> u32;
    fn SetBkColor(dc: *mut c_void, color: u32) -> u32;
    /// `FillRect` would be the obvious call, but `gutter` already declares it against its own
    /// rectangle type, and a second declaration of the same entry point with a different signature
    /// is a warning this build does not carry. `PatBlt` with a brush selected paints the same
    /// rectangle and is declared only here.
    fn PatBlt(
        dc: *mut c_void,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        raster: u32,
    ) -> i32;
}

/// `PATCOPY`: paint the selected brush, nothing clever.
const PATCOPY: u32 = 0x00F0_0089;

#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: *mut c_void,
        attribute: u32,
        value: *const c_void,
        size: u32,
    ) -> i32;
}

extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *const c_void;
}

/// `uxtheme.dll` ordinal 135, `SetPreferredAppMode`. It has no header, no import library and no
/// name to look up, which is how Microsoft marks an entry point that is not offered to us: the
/// library is loaded and the ordinal resolved at run time, and a Windows without it answers null,
/// this program goes on drawing itself light, and nothing about the shell is affected.
///
/// An ordinal is not a string. `GetProcAddress` tells the two apart by whether the high word of the
/// pointer is zero, so the ordinal is passed as a pointer whose value is the number - which is what
/// `MAKEINTRESOURCE` spells in the C headers, and why the cast below looks as odd as it does.
const SET_PREFERRED_APP_MODE: u64 = 135;

/// `SetPreferredAppMode`'s `ForceDark`: comctl32 draws the buttons, the tab strip, the scroll bars
/// and the rest of the common controls in their dark variants.
const MODE_FORCE_DARK: i32 = 2;

unsafe fn set_preferred_app_mode(mode: i32) {
    let name = wide("uxtheme.dll");
    // A plain load, not `LOAD_LIBRARY_AS_DATAFILE`: the library is already in the process for the
    // visual styles, and mapping it as data would leave its code uncallable.
    let library = LoadLibraryExW(name.as_ptr(), std::ptr::null_mut(), 0);
    if library.is_null() {
        return;
    }
    let address = GetProcAddress(library, SET_PREFERRED_APP_MODE as *const u8);
    if address.is_null() {
        return;
    }
    let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(address);
    set_mode(mode);
}

/// The messages this module answers, all of them sent by a child control before it paints.
pub const WM_ERASEBKGND: u32 = 0x0014;
pub const WM_CTLCOLORSTATIC: u32 = 0x0138;
pub const WM_CTLCOLOREDIT: u32 = 0x0133;
pub const WM_CTLCOLORLISTBOX: u32 = 0x0134;
const WM_CTLCOLORBTN: u32 = 0x0135;
pub const WM_CTLCOLORDLG: u32 = 0x0136;
const WM_CTLCOLORSCROLLBAR: u32 = 0x0137;

const HKEY_CURRENT_USER: *const c_void = -2147483647isize as *const c_void;
/// `RRF_RT_REG_DWORD`: read the value only if it is a DWORD, which is what the theme key holds.
/// The number is 0x10 and not 0x4 - 0x4 is `RRF_RT_REG_EXPAND_SZ`, and asking for a string from a
/// key that holds a number makes the call report `ERROR_BAD_DATA`, which in this program's first
/// test silently meant "light mode" and a dark desktop that drew a white window.
const RRF_RT_DWORD: u32 = 0x0010;

/// A colour as the window manager wants it: red, green, blue in that order.
pub type Rgb = [u8; 3];

/// The eight colours the script pane's syntax rules use. The light set is the one this program has
/// always had, kept exactly, so a machine in light mode sees the window it saw in 1.0.9.
#[derive(Clone, Copy)]
pub struct Syntax {
    pub text: Rgb,
    pub comment: Rgb,
    pub string: Rgb,
    pub variable: Rgb,
    pub parameter: Rgb,
    pub number: Rgb,
    pub keyword: Rgb,
    pub command: Rgb,
}

/// Everything the window is painted with, plus the brushes made from it. The brushes are created
/// once and never destroyed: they live as long as the process, which is the same bargain the rest
/// of this program makes with GDI objects it hands to Windows to keep.
#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    /// The gaps between the panes, and the face of the toolbar.
    pub window_bg: Rgb,
    /// The field of an edit box, the inside of a list box.
    pub control_bg: Rgb,
    pub control_fg: Rgb,
    /// The script pane.
    pub editor_bg: Rgb,
    pub gutter_bg: Rgb,
    pub gutter_fg: Rgb,
    pub gutter_sep: Rgb,
    /// The dot a line with a breakpoint carries. It is the one thing in the margin that is not
    /// a number or a rule, so it is also the one place a colour can mean something on its own.
    pub breakpoint: Rgb,
    /// A breakpoint that is there but not armed. Dimmed rather than hidden, because forgetting
    /// about a breakpoint you switched off is how a script gets blamed for running straight
    /// through it.
    pub breakpoint_off: Rgb,
    /// The number of the line execution is held on, and the band behind it.
    pub stop_fg: Rgb,
    pub stop_bg: Rgb,
    pub syntax: Syntax,

    window_brush: *mut c_void,
    control_brush: *mut c_void,
}

static PALETTE: OnceLock<Palette> = OnceLock::new();

// The palette holds two brush handles, which is why it is not `Sync` on its own. It is safe to
// share: both are made once, in `Palette::load`, before the first window exists, and nothing after
// that writes to the palette or deletes them - Windows keeps using them to paint with, and a brush
// is a immutable handle to a colour as far as this program is concerned. Every read happens on the
// UI thread in any case, and the claim is here so the `OnceLock` can be a static rather than a
// thread-local that each caller has to fill in again.
unsafe impl Sync for Palette {}
unsafe impl Send for Palette {}

/// The palette, built the first time it is asked for - which is during `build_ui`, before any
/// control exists, so that the theme switch reaches the common controls while they are being made.
pub fn palette() -> &'static Palette {
    PALETTE.get_or_init(Palette::load)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

impl Palette {
    fn load() -> Palette {
        let dark = apps_use_dark_mode();
        // Set it before a single control is created: comctl32 decides how to draw a control when
        // the control is created, so a window told to go dark afterwards keeps light buttons.
        if dark {
            unsafe { set_preferred_app_mode(MODE_FORCE_DARK) };
        }
        let palette = if dark { Palette::dark() } else { Palette::light() };
        palette.make_brushes()
    }

    /// The setting behind Start ▸ Personalisation ▸ Colours ▸ "Choose your default app mode".
    /// Absent means light, which is what Windows itself does with it.
    fn app_mode_is_dark() -> bool {
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let value = wide("AppsUseLightTheme");
        let mut data: u32 = 1;
        let mut length = std::mem::size_of::<u32>() as u32;
        let code = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_DWORD,
                std::ptr::null_mut(),
                &mut data as *mut u32 as *mut u8,
                &mut length,
            )
        };
        if code != 0 {
            return false;
        }
        data == 0
    }

    fn light() -> Palette {
        Palette {
            dark: false,
            window_bg: [0xF0, 0xF0, 0xF0],
            control_bg: [0xFF, 0xFF, 0xFF],
            control_fg: [0x00, 0x00, 0x00],
            editor_bg: [0xFF, 0xFF, 0xFF],
            gutter_bg: [0xFF, 0xFF, 0xFF],
            gutter_fg: [0xA0, 0xA0, 0xA0],
            gutter_sep: [0xDC, 0xDC, 0xDC],
            breakpoint: [0xC5, 0x3A, 0x3A],
            breakpoint_off: [0xD6, 0xB4, 0xB4],
            stop_fg: [0x8A, 0x5A, 0x00],
            stop_bg: [0xFF, 0xF0, 0xC8],
            syntax: Syntax {
                text: [0x00, 0x00, 0x00],
                comment: [0x00, 0x80, 0x00],
                string: [0x8B, 0x00, 0x00],
                variable: [0xC0, 0x40, 0x00],
                parameter: [0x00, 0x72, 0x9B],
                number: [0x80, 0x00, 0x80],
                keyword: [0x00, 0x00, 0xFF],
                command: [0x00, 0x00, 0x8B],
            },
            window_brush: std::ptr::null_mut(),
            control_brush: std::ptr::null_mut(),
        }
    }

    /// The dark set, with the console's own Campbell colours where the two surfaces meet, so the
    /// window around the console reads as the same window rather than a frame someone painted.
    fn dark() -> Palette {
        Palette {
            dark: true,
            window_bg: [0x20, 0x20, 0x20],
            control_bg: [0x2B, 0x2B, 0x2B],
            control_fg: [0xDC, 0xDC, 0xDC],
            editor_bg: [0x1F, 0x1F, 0x1F],
            gutter_bg: [0x1F, 0x1F, 0x1F],
            gutter_fg: [0x85, 0x85, 0x85],
            gutter_sep: [0x3F, 0x3F, 0x3F],
            breakpoint: [0xF0, 0x5A, 0x5A],
            breakpoint_off: [0x6E, 0x4A, 0x4A],
            stop_fg: [0xE8, 0xB8, 0x4A],
            stop_bg: [0x3A, 0x33, 0x1E],
            syntax: Syntax {
                text: [0xDC, 0xDC, 0xDC],
                comment: [0x6A, 0x99, 0x55],
                string: [0xCE, 0x91, 0x78],
                variable: [0x9C, 0xDC, 0xFE],
                parameter: [0x56, 0x9C, 0xD6],
                number: [0xB5, 0xCE, 0xA8],
                keyword: [0x56, 0x9C, 0xD6],
                command: [0xDC, 0xDC, 0xAA],
            },
            window_brush: std::ptr::null_mut(),
            control_brush: std::ptr::null_mut(),
        }
    }

    fn make_brushes(mut self) -> Palette {
        self.window_brush = brush(self.window_bg);
        self.control_brush = brush(self.control_bg);
        self
    }

    /// Answers one of the `WM_CTLCOLOR*` messages a child control sends before it paints: the text
    /// and the field it sits in, then the brush Windows uses to clear the rest. Returns `None` in
    /// light mode so the control paints itself the way it always has.
    ///
    /// `hdc` is the device context Windows hands over, already bound to the child.
    pub fn child_colour(&self, message: u32, dc: *mut c_void) -> Option<isize> {
        if !self.dark || dc.is_null() {
            return None;
        }
        let (fg, bg) = match message {
            WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => (self.control_fg, self.control_bg),
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORDLG | WM_CTLCOLORSCROLLBAR => {
                (self.control_fg, self.window_bg)
            }
            _ => return None,
        };
        unsafe {
            SetTextColor(dc, colorref(fg) as u32);
            SetBkColor(dc, colorref(bg) as u32);
        }
        // The static label and the list boxes sit on the window's face; the edits sit on theirs.
        let brush = match message {
            WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => self.control_brush,
            _ => self.window_brush,
        };
        Some(brush as isize)
    }

    /// Paints the background of the frame itself. nwg registers its window class with
    /// `COLOR_WINDOW`, a system colour index that stays white in dark mode, so without this the
    /// space between the panes flashes white on every resize.
    ///
    /// The rectangle has to be measured: a paint call handed a null one paints nothing and reports
    /// nothing, which is how a background that only looks implemented gets shipped.
    pub fn erase_background(&self, hwnd: *mut c_void, dc: *mut c_void) -> Option<isize> {
        if !self.dark || hwnd.is_null() || dc.is_null() {
            return None;
        }
        let mut rect = [0i32; 4];
        unsafe {
            if GetClientRect(hwnd, rect.as_mut_ptr()) == 0 {
                return None;
            }
            let old = SelectObject(dc, self.window_brush);
            PatBlt(dc, 0, 0, rect[2] - rect[0], rect[3] - rect[1], PATCOPY);
            if !old.is_null() {
                SelectObject(dc, old);
            }
        }
        Some(1)
    }
}

/// Whether this program should draw itself dark. Kept as a free function so `build_ui` can ask
/// without holding a palette.
pub fn apps_use_dark_mode() -> bool {
    Palette::app_mode_is_dark()
}

fn brush(rgb: Rgb) -> *mut c_void {
    unsafe { CreateSolidBrush(colorref(rgb) as u32) }
}

/// Tells the desktop window manager to draw the title bar and its buttons dark. Called once the
/// window exists, because unlike the control styles this is per window and it can be changed at
/// any time.
pub fn apply_titlebar(hwnd: *mut c_void) {
    if hwnd.is_null() || !palette().dark {
        return;
    }
    let on: i32 = 1;
    for attribute in [20u32, 19] {
        // 20 is DWMWA_USE_IMMERSIVE_DARK_MODE; 19 is the same setting under its number before
        // Windows 10 2004. One of the two exists on any machine this program runs on, and the
        // other answers an error code that means "no", which is safe to ignore.
        let code =
            unsafe { DwmSetWindowAttribute(hwnd, attribute, &on as *const i32 as *const c_void, 4) };
        if code == 0 {
            break;
        }
    }
}
