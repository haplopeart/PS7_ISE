//! The application icon.
//!
//! `build.rs` compiles `assets\ps7ise.ico` - the PowerShell 7 logo taken out of
//! `pwsh.exe` - into the exe as its ICON resource, which is what Explorer, the taskbar and
//! Alt-Tab show. The title bar could be given a second, separate copy of the image, and
//! then the two would drift apart the moment one was updated; instead the window asks this
//! very module for the icon it already carries and passes that handle to `WM_SETICON`.

use crate::console::send_message;
use std::ffi::c_void;

extern "system" {
    fn GetModuleHandleW(module_name: *const u16) -> *mut c_void;
    fn LoadImageW(
        module: *mut c_void,
        name: *const u16,
        image_type: u32,
        width: i32,
        height: i32,
        flags: u32,
    ) -> *mut c_void;
    fn SetWindowPos(
        window: *mut c_void,
        insert_after: *mut c_void,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        flags: u32,
    ) -> i32;
}

const WM_SETICON: u32 = 0x0080;
const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;
const IMAGE_ICON: u32 = 1;
const LR_DEFAULTCOLOR: u32 = 0x0000;

/// `MAKEINTRESOURCEW(1)`: the first ICON resource in the file, which is what
/// `winresource` writes the application icon as. The ordinal is passed as a pointer.
const APP_ICON: *const u16 = 1usize as *const u16;

/// Loads the exe's own icon at one of the sizes the .ico provides. The handle is kept for
/// the life of the process on purpose - the window still points at it after this returns,
/// so it is never destroyed.
fn load(size: i32) -> Option<*mut c_void> {
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    if module.is_null() {
        return None;
    }
    let icon =
        unsafe { LoadImageW(module, APP_ICON, IMAGE_ICON, size, size, LR_DEFAULTCOLOR) };
    if icon.is_null() {
        None
    } else {
        Some(icon)
    }
}

/// Puts the icon in the corner of the title bar (16 px) and on the taskbar button and
/// Alt-Tab entry (32 px). False means the exe carries no icon resource, which is the case
/// when the build machine had no Windows SDK for `rc.exe`.
pub fn apply(hwnd: *mut c_void) -> bool {
    let mut applied = false;
    for (which, size) in [(ICON_SMALL, 16), (ICON_BIG, 32)] {
        if let Some(icon) = load(size) {
            send_message(hwnd, WM_SETICON, which, icon as isize);
            applied = true;
        }
    }
    if applied {
        // The window is already on screen, and it will not redraw the title bar on its
        // own just because an icon appeared. A frame change with nothing else changed is
        // the way to ask for that repaint.
        const SWP_NOSIZE: u32 = 0x0001;
        const SWP_NOMOVE: u32 = 0x0002;
        const SWP_NOZORDER: u32 = 0x0004;
        const SWP_FRAMECHANGED: u32 = 0x0020;
        unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOMOVE | SWP_NOZORDER | SWP_FRAMECHANGED,
            )
        };
    }
    applied
}
