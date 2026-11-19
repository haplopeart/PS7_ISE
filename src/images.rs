//! The pictures the toolbar buttons carry.
//!
//! The original ISE says what its buttons do with a small picture and a tooltip, not with a
//! word: a page for New, a folder for Open, a floppy for Save, a green arrow for Run. This
//! module draws those pictures, and the reasons it draws them rather than getting them from
//! somewhere else are worth writing down:
//!
//! * The original pictures cannot be used. They live in `powershell_ise.exe`, which is
//!   Microsoft's, and this repository is MIT.
//! * Nor are they read out of a system DLL. The resource numbers in `shell32.dll` and
//!   `user32.dll` are not a documented interface and they do move between versions of Windows;
//!   a toolbar that draws a printer where Save belongs is worse than one with words in it. The
//!   Administrator shield is the toolbar's one exception, in `icons`, because that resource is
//!   what Explorer itself uses and has not moved.
//! * Drawing keeps the build exactly as it was: no image file to ship, no decoder, no crate.
//!   All it needs is GDI+, which is part of Windows.
//!
//! A picture is drawn into a 32-bit ARGB bitmap that starts fully transparent, so GDI+'s
//! antialiasing blends into nothing instead of leaving a grey box around the icon, and is then
//! turned into a real icon by `CreateIconIndirect`. A button is handed that handle the same way
//! the shield already is, through `BM_SETIMAGE`.
//!
//! The handles are deliberately never destroyed. The buttons are still pointing at their
//! pictures when these functions return, and giving one back while that is true is the bug, not
//! the cleanup.

use crate::console::send_message;
use std::ffi::c_void;
use std::sync::Once;

#[link(name = "gdiplus")]
extern "system" {
    fn GdiplusStartup(token: *mut usize, input: *const StartupInput, output: *mut c_void) -> i32;
    fn GdipCreateBitmapFromScan0(
        width: i32,
        height: i32,
        stride: i32,
        format: u32,
        pixels: *mut u8,
        image: *mut *mut c_void,
    ) -> i32;
    /// The C entry point behind `Graphics::FromImage`. The C++ name is not exported, so this is
    /// the only way to draw on a bitmap that was made rather than loaded.
    fn GdipGetImageGraphicsContext(image: *mut c_void, graphics: *mut *mut c_void) -> i32;
    fn GdipSetSmoothingMode(graphics: *mut c_void, mode: i32) -> i32;
    fn GdipCreateSolidFill(color: u32, brush: *mut *mut c_void) -> i32;
    fn GdipCreatePen1(color: u32, width: f32, unit: i32, pen: *mut *mut c_void) -> i32;
    fn GdipFillRectangle(
        graphics: *mut c_void,
        brush: *mut c_void,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> i32;
    fn GdipDrawRectangle(
        graphics: *mut c_void,
        pen: *mut c_void,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> i32;
    fn GdipFillEllipse(
        graphics: *mut c_void,
        brush: *mut c_void,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> i32;
    fn GdipDrawEllipse(
        graphics: *mut c_void,
        pen: *mut c_void,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> i32;
    fn GdipFillPolygon(
        graphics: *mut c_void,
        brush: *mut c_void,
        points: *const Point,
        count: i32,
        mode: i32,
    ) -> i32;
    fn GdipDrawPolygon(
        graphics: *mut c_void,
        pen: *mut c_void,
        points: *const Point,
        count: i32,
    ) -> i32;
    fn GdipDrawLine(
        graphics: *mut c_void,
        pen: *mut c_void,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    ) -> i32;
    fn GdipCreateFontFamilyFromName(
        name: *const u16,
        collection: *mut c_void,
        family: *mut *mut c_void,
    ) -> i32;
    fn GdipCreateFont(
        family: *mut c_void,
        em_size: f32,
        style: i32,
        unit: i32,
        font: *mut *mut c_void,
    ) -> i32;
    fn GdipCreateStringFormat(flags: i32, language: u16, format: *mut *mut c_void) -> i32;
    fn GdipSetStringFormatAlign(format: *mut c_void, align: i32) -> i32;
    fn GdipSetStringFormatLineAlign(format: *mut c_void, align: i32) -> i32;
    fn GdipDrawString(
        graphics: *mut c_void,
        text: *const u16,
        length: i32,
        font: *mut c_void,
        layout: *const Rect,
        format: *mut c_void,
        brush: *mut c_void,
    ) -> i32;
    fn GdipCreateHBITMAPFromBitmap(
        image: *mut c_void,
        bitmap: *mut *mut c_void,
        background: u32,
    ) -> i32;
    fn GdipDeleteGraphics(graphics: *mut c_void) -> i32;
    fn GdipDeleteBrush(brush: *mut c_void) -> i32;
    fn GdipDeletePen(pen: *mut c_void) -> i32;
    fn GdipDeleteFont(font: *mut c_void) -> i32;
    fn GdipDeleteFontFamily(family: *mut c_void) -> i32;
    fn GdipDeleteStringFormat(format: *mut c_void) -> i32;
    fn GdipDisposeImage(image: *mut c_void) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateBitmap(
        width: i32,
        height: i32,
        planes: u32,
        bits_per_plane: u32,
        bits: *const c_void,
    ) -> *mut c_void;
    fn DeleteObject(object: *mut c_void) -> i32;
}

#[link(name = "user32")]
extern "system" {
    fn CreateIconIndirect(info: *const IconInfo) -> *mut c_void;
}

/// `GdiplusStartupInput`. Version 1.0 is enough for everything used here; the two flags after
/// it are left off, which means GDI+ runs the background thread it likes to run.
#[repr(C)]
struct StartupInput {
    version: u32,
    debug_callback: *mut c_void,
    suppress_background_thread: i32,
    suppress_external_codecs: i32,
}

/// `Gdiplus::Point` - two single-precision coordinates, because every GDI+ drawing call works in
/// floats and 16-pixel pictures are full of halves.
#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
    x: f32,
    y: f32,
}

/// `Gdiplus::RectF`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// `ICONINFO`. The two bitmaps are what an icon is made of; with `use_mask` clear, the colour
/// bitmap's alpha channel is the mask, which is what makes the edges of these pictures smooth
/// rather than staircased.
#[repr(C)]
struct IconInfo {
    is_icon: i32,
    use_mask: i32,
    hotspot_x: u32,
    hotspot_y: u32,
    mask: *mut c_void,
    colour: *mut c_void,
}

/// GDI+ answers with a status, and 0 is `Ok`. Every call here is drawing, so a failure is a
/// picture that is missing a piece, not something to report to the user.
const OK: i32 = 0;
/// `PixelFormat32bppARGB`.
const PIXEL_FORMAT: u32 = 0x0026_200A;
/// `SmoothingModeAntiAlias`. Without it a diagonal edge at 16 pixels is a staircase.
const SMOOTHING: i32 = 2;
/// `FillModeWinding`.
const FILL_WINDING: i32 = 0;
/// `UnitPixel`, so the pen widths in this file are in picture pixels.
const UNIT_PIXEL: i32 = 2;
/// `FontStyleBold`.
const FONT_BOLD: i32 = 1;
/// `StringFormatFlagsDirectionRightToLeft` is not wanted; this is the value that means "a plain
/// format", which is then told to centre.
const FORMAT_PLAIN: i32 = 0;
/// `StringAlignmentCenter`.
const ALIGN_CENTER: i32 = 1;
const BM_SETIMAGE: u32 = 0x00F7;
const IMAGE_ICON: usize = 1;

/// The edge of a picture, in pixels. Every shape below is drawn inside this box.
const SIDE: i32 = 16;

/// What a button shows. Named for what the button does, because that is the question asked when
/// one of these is added or taken away.
#[derive(Clone, Copy)]
pub enum Picture {
    New,
    Open,
    Save,
    RunScript,
    RunSelection,
    Stop,
    ClearPane,
    Help,
}

/// Starts GDI+. The token is never handed back on purpose: the pictures live as long as the
/// window, and shutting GDI+ down while they exist would be the mistake.
static START: Once = Once::new();
static mut USABLE: bool = false;

fn startup() -> bool {
    START.call_once(|| {
        let input = StartupInput {
            version: 1,
            debug_callback: std::ptr::null_mut(),
            suppress_background_thread: 0,
            suppress_external_codecs: 0,
        };
        let mut token = 0usize;
        unsafe {
            USABLE = GdiplusStartup(&mut token, &input, std::ptr::null_mut()) == OK;
        }
    });
    unsafe { USABLE }
}

/// Draws one picture and turns it into an icon. `None` means the pictures cannot be had at all -
/// no GDI+ on the machine - and the toolbar has to fall back to words.
pub fn create(picture: Picture) -> Option<*mut c_void> {
    if !startup() {
        return None;
    }

    let stride = SIDE * 4;
    let mut pixels = vec![0u8; (stride * SIDE) as usize];
    let mut image = std::ptr::null_mut();
    if unsafe {
        GdipCreateBitmapFromScan0(
            SIDE,
            SIDE,
            stride,
            PIXEL_FORMAT,
            pixels.as_mut_ptr(),
            &mut image,
        )
    } != OK
        || image.is_null()
    {
        return None;
    }

    let mut graphics = std::ptr::null_mut();
    if unsafe { GdipGetImageGraphicsContext(image, &mut graphics) } != OK || graphics.is_null() {
        unsafe { GdipDisposeImage(image) };
        return None;
    }

    unsafe { GdipSetSmoothingMode(graphics, SMOOTHING) };
    draw(&Painter { graphics }, picture);
    unsafe { GdipDeleteGraphics(graphics) };

    // The bitmap above is GDI+'s view of the buffer; this is the GDI object `CreateIconIndirect`
    // wants, and it copies, so the buffer can go as soon as it exists.
    let mut colour = std::ptr::null_mut();
    let made = unsafe { GdipCreateHBITMAPFromBitmap(image, &mut colour, 0) } == OK;
    unsafe { GdipDisposeImage(image) };
    if !made || colour.is_null() {
        return None;
    }

    // The mask is still required even though nothing reads it: `CreateIconIndirect` fails without
    // one. It is monochrome and empty, which is the shape the alpha channel is asked to ignore.
    let mask = unsafe { CreateBitmap(SIDE, SIDE, 1, 1, std::ptr::null()) };
    let info = IconInfo {
        is_icon: 1,
        use_mask: 0,
        hotspot_x: 0,
        hotspot_y: 0,
        mask,
        colour,
    };
    let icon = unsafe { CreateIconIndirect(&info) };

    // Both bitmaps were copied into the icon, so these two are ours to give back. The icon is
    // not: the button is pointing at it.
    unsafe {
        if !mask.is_null() {
            DeleteObject(mask);
        }
        DeleteObject(colour);
    }

    if icon.is_null() {
        None
    } else {
        Some(icon)
    }
}

/// Puts a picture on a button. The button has to have been created with `ButtonFlags::ICON`, or
/// it will draw its (empty) caption instead of the picture.
pub fn set(button: *mut c_void, icon: *mut c_void) {
    send_message(button, BM_SETIMAGE, IMAGE_ICON, icon as isize);
}

/// A colour: GDI+ takes ARGB as one 32-bit number, most easily written in the order it names it.
const WHITE: u32 = 0xFFFF_FFFF;
/// The paper a blank script is drawn on.
const PAPER: u32 = 0xFFF7_F7F5;
/// ... and the line that keeps it on the button.
const EDGE: u32 = 0xFF7A_7A7A;
/// The turned-down corner of a page.
const FOLD: u32 = 0xFFD2_D2CE;
/// Green is the colour every version of the ISE has used for "run", so the run buttons take it.
const GREEN: u32 = 0xFF2E_9B3E;
const GREEN_EDGE: u32 = 0xFF1C_6B29;
const RED: u32 = 0xFFC8_382E;
const RED_EDGE: u32 = 0xFF7C_1D16;
/// The floppy disk, as blue as the ones that were blue.
const DISK: u32 = 0xFF3F_6FB5;
const DISK_EDGE: u32 = 0xFF24_446E;
const METAL: u32 = 0xFFD6_DCE4;
/// The folder, and the lighter front flap that makes it an open one.
const AMBER: u32 = 0xFFE0_A93E;
const AMBER_EDGE: u32 = 0xFF8A_6318;
const AMBER_LIGHT: u32 = 0xFFFF_D066;
/// Selected lines, translucent the way a real selection is.
const SELECT: u32 = 0x663F_86D6;
/// The eraser that clears the pane: a pink body and the face that touches the page.
const PINK: u32 = 0xFFE0_707F;
const PINK_EDGE: u32 = 0xFF8E_3A48;
const RUBBER: u32 = 0xFF9F_C6E8;
const RUBBER_EDGE: u32 = 0xFF47_6F99;
/// The blue disc the help mark sits on.
const BADGE: u32 = 0xFF2A_6FD0;
const BADGE_EDGE: u32 = 0xFF1A_4A8F;

/// One picture being drawn. Each call makes the brush or pen it needs, uses it once, and gives it
/// back: nine pictures are built once at start-up, so the cost is nothing, and a drawing list that
/// reads as "fill this shape with this colour" is worth more here than a cache.
struct Painter {
    graphics: *mut c_void,
}

/// A point, spelled the way a picture is read.
fn point(x: f32, y: f32) -> Point {
    Point { x, y }
}

/// The four corners of a rectangle turned about its centre. The eraser is a box at 45 degrees,
/// and working its corners out by hand for each of its two parts is how a picture like that ends
/// up crooked.
fn tilted(cx: f32, cy: f32, half_width: f32, half_height: f32, degrees: f32) -> [Point; 4] {
    let angle = degrees * std::f32::consts::PI / 180.0;
    let (sin, cos) = angle.sin_cos();
    let corner = |x: f32, y: f32| Point {
        x: cx + x * cos - y * sin,
        y: cy + x * sin + y * cos,
    };
    [
        corner(-half_width, -half_height),
        corner(half_width, -half_height),
        corner(half_width, half_height),
        corner(-half_width, half_height),
    ]
}

impl Painter {
    fn fill(&self, colour: u32, corners: &[Point]) {
        let mut brush = std::ptr::null_mut();
        if unsafe { GdipCreateSolidFill(colour, &mut brush) } != OK || brush.is_null() {
            return;
        }
        unsafe {
            GdipFillPolygon(
                self.graphics,
                brush,
                corners.as_ptr(),
                corners.len() as i32,
                FILL_WINDING,
            )
        };
        unsafe { GdipDeleteBrush(brush) };
    }

    fn outline(&self, colour: u32, width: f32, corners: &[Point]) {
        let mut pen = std::ptr::null_mut();
        if unsafe { GdipCreatePen1(colour, width, UNIT_PIXEL, &mut pen) } != OK || pen.is_null() {
            return;
        }
        unsafe { GdipDrawPolygon(self.graphics, pen, corners.as_ptr(), corners.len() as i32) };
        unsafe { GdipDeletePen(pen) };
    }

    fn fill_rect(&self, colour: u32, x: f32, y: f32, width: f32, height: f32) {
        let mut brush = std::ptr::null_mut();
        if unsafe { GdipCreateSolidFill(colour, &mut brush) } != OK || brush.is_null() {
            return;
        }
        unsafe { GdipFillRectangle(self.graphics, brush, x, y, width, height) };
        unsafe { GdipDeleteBrush(brush) };
    }

    fn outline_rect(&self, colour: u32, width: f32, x: f32, y: f32, w: f32, h: f32) {
        let mut pen = std::ptr::null_mut();
        if unsafe { GdipCreatePen1(colour, width, UNIT_PIXEL, &mut pen) } != OK || pen.is_null() {
            return;
        }
        unsafe { GdipDrawRectangle(self.graphics, pen, x, y, w, h) };
        unsafe { GdipDeletePen(pen) };
    }

    fn fill_circle(&self, colour: u32, cx: f32, cy: f32, radius: f32) {
        let mut brush = std::ptr::null_mut();
        if unsafe { GdipCreateSolidFill(colour, &mut brush) } != OK || brush.is_null() {
            return;
        }
        unsafe {
            GdipFillEllipse(
                self.graphics,
                brush,
                cx - radius,
                cy - radius,
                radius * 2.0,
                radius * 2.0,
            )
        };
        unsafe { GdipDeleteBrush(brush) };
    }

    fn outline_circle(&self, colour: u32, width: f32, cx: f32, cy: f32, radius: f32) {
        let mut pen = std::ptr::null_mut();
        if unsafe { GdipCreatePen1(colour, width, UNIT_PIXEL, &mut pen) } != OK || pen.is_null() {
            return;
        }
        unsafe {
            GdipDrawEllipse(
                self.graphics,
                pen,
                cx - radius,
                cy - radius,
                radius * 2.0,
                radius * 2.0,
            )
        };
        unsafe { GdipDeletePen(pen) };
    }

    fn line(&self, colour: u32, width: f32, x1: f32, y1: f32, x2: f32, y2: f32) {
        let mut pen = std::ptr::null_mut();
        if unsafe { GdipCreatePen1(colour, width, UNIT_PIXEL, &mut pen) } != OK || pen.is_null() {
            return;
        }
        unsafe { GdipDrawLine(self.graphics, pen, x1, y1, x2, y2) };
        unsafe { GdipDeletePen(pen) };
    }

    /// Writes a string, centred in the box it is given. Only the help mark uses this, and a
    /// question mark drawn with a font is a question mark; drawn with line segments it is a
    /// smudge. The UI font is asked for first and a system one after it, because GDI+ has no
    /// entry point that means "whatever font you happen to have".
    fn write(&self, colour: u32, size: f32, text: &str, layout: Rect) {
        let mut family = std::ptr::null_mut();
        for name in ["Segoe UI", "Microsoft Sans Serif"] {
            let name = wide(name);
            if unsafe {
                GdipCreateFontFamilyFromName(name.as_ptr(), std::ptr::null_mut(), &mut family)
            } == OK
                && !family.is_null()
            {
                break;
            }
            family = std::ptr::null_mut();
        }
        if family.is_null() {
            return;
        }

        let mut font = std::ptr::null_mut();
        let got_font =
            unsafe { GdipCreateFont(family, size, FONT_BOLD, UNIT_PIXEL, &mut font) } == OK;
        unsafe { GdipDeleteFontFamily(family) };
        if !got_font || font.is_null() {
            return;
        }

        let mut format = std::ptr::null_mut();
        let got_format = unsafe { GdipCreateStringFormat(FORMAT_PLAIN, 0, &mut format) } == OK
            && !format.is_null();
        if got_format {
            unsafe {
                GdipSetStringFormatAlign(format, ALIGN_CENTER);
                GdipSetStringFormatLineAlign(format, ALIGN_CENTER);
            }
        }

        let text = wide(text);
        let mut brush = std::ptr::null_mut();
        if unsafe { GdipCreateSolidFill(colour, &mut brush) } == OK && !brush.is_null() {
            unsafe {
                GdipDrawString(
                    self.graphics,
                    text.as_ptr(),
                    -1, // -1: read the string up to its terminator
                    font,
                    &layout,
                    if got_format { format } else { std::ptr::null_mut() },
                    brush,
                )
            };
            unsafe { GdipDeleteBrush(brush) };
        }

        if got_format {
            unsafe { GdipDeleteStringFormat(format) };
        }
        unsafe { GdipDeleteFont(font) };
    }
}

/// A control's text as a win32 wide string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The green disc with a white cross: "make a new one". Drawn over the corner of whatever it
/// belongs to, which is how every version of Windows shows a new item.
fn new_badge(painter: &Painter, cx: f32, cy: f32, radius: f32) {
    painter.fill_circle(GREEN, cx, cy, radius);
    painter.outline_circle(GREEN_EDGE, 1.0, cx, cy, radius);
    let arm = radius * 0.55;
    painter.line(WHITE, 1.6, cx - arm, cy, cx + arm, cy);
    painter.line(WHITE, 1.6, cx, cy - arm, cx, cy + arm);
}

/// The green arrow: run.
fn play(painter: &Painter, left: f32, top: f32, right: f32, bottom: f32) {
    let middle_y = (top + bottom) / 2.0;
    let arrow = [
        point(left, top),
        point(right, middle_y),
        point(left, bottom),
    ];
    painter.fill(GREEN, &arrow);
    painter.outline(GREEN_EDGE, 1.0, &arrow);
}

/// Draws one picture. The shapes are all in the 16 by 16 box this module's pictures are made in,
/// with the 0.5 offsets that keep a one-pixel line from landing across two device rows.
fn draw(painter: &Painter, picture: Picture) {
    match picture {
        // A page with its corner turned down, and the mark for a new one on top of it.
        Picture::New => {
            let page = [
                point(3.0, 1.5),
                point(9.5, 1.5),
                point(13.0, 5.0),
                point(13.0, 14.5),
                point(3.0, 14.5),
            ];
            painter.fill(PAPER, &page);
            painter.outline(EDGE, 1.0, &page);
            let fold = [point(9.5, 1.5), point(13.0, 5.0), point(9.5, 5.0)];
            painter.fill(FOLD, &fold);
            painter.outline(EDGE, 1.0, &fold);
            new_badge(painter, 11.2, 11.2, 3.9);
        }
        // A folder with its flap open, which is what "open" has meant on a toolbar for decades.
        Picture::Open => {
            let back = [
                point(2.0, 4.0),
                point(6.2, 4.0),
                point(7.8, 6.0),
                point(14.0, 6.0),
                point(14.0, 13.5),
                point(2.0, 13.5),
            ];
            painter.fill(AMBER, &back);
            painter.outline(AMBER_EDGE, 1.0, &back);
            let flap = [
                point(3.6, 13.5),
                point(5.8, 7.4),
                point(15.2, 7.4),
                point(13.0, 13.5),
            ];
            painter.fill(AMBER_LIGHT, &flap);
            painter.outline(AMBER_EDGE, 1.0, &flap);
        }
        // A floppy disk: the case, the shutter, the label. Nobody under forty will know what it
        // is, and everyone will still read it as save.
        Picture::Save => {
            painter.fill(DISK, &[
                point(2.0, 2.0),
                point(14.0, 2.0),
                point(14.0, 14.0),
                point(2.0, 14.0),
            ]);
            painter.outline_rect(DISK_EDGE, 1.0, 2.0, 2.0, 12.0, 12.0);
            painter.fill_rect(METAL, 4.2, 2.5, 7.6, 4.4);
            // The slot in the shutter, in the colour of the case behind it.
            painter.fill_rect(DISK, 9.4, 3.1, 1.7, 3.2);
            painter.fill_rect(PAPER, 4.2, 8.4, 7.6, 5.2);
            painter.line(EDGE, 1.0, 5.2, 10.4, 10.8, 10.4);
        }
        // Run the whole script.
        Picture::RunScript => play(painter, 4.0, 2.0, 13.5, 14.0),
        // The same arrow, smaller, beside the lines it will actually run.
        Picture::RunSelection => {
            let page = [
                point(2.5, 1.5),
                point(8.0, 1.5),
                point(11.0, 4.5),
                point(11.0, 14.5),
                point(2.5, 14.5),
            ];
            painter.fill(PAPER, &page);
            painter.outline(EDGE, 1.0, &page);
            painter.fill_rect(SELECT, 4.0, 6.3, 5.4, 2.9);
            play(painter, 9.6, 8.6, 14.9, 15.2);
        }
        // Stop. The original uses a grey square; red says "this one is worth noticing" and costs
        // nothing, and the shape is the same one every media control has used since.
        Picture::Stop => {
            painter.fill(RED, &[
                point(3.4, 3.4),
                point(12.6, 3.4),
                point(12.6, 12.6),
                point(3.4, 12.6),
            ]);
            painter.outline_rect(RED_EDGE, 1.0, 3.4, 3.4, 9.2, 9.2);
        }
        // An eraser over the line it has just wiped, for a pane that is cleared rather than
        // deleted.
        Picture::ClearPane => {
            let body = tilted(10.2, 6.2, 2.9, 3.4, 45.0);
            painter.fill(PINK, &body);
            painter.outline(PINK_EDGE, 1.0, &body);
            let face = tilted(6.9, 9.5, 2.9, 1.3, 45.0);
            painter.fill(RUBBER, &face);
            painter.outline(RUBBER_EDGE, 1.0, &face);
            painter.line(EDGE, 1.0, 2.4, 14.2, 8.6, 14.2);
        }
        // Help is a blue disc with a question mark on it, because that is the shape of help.
        Picture::Help => {
            painter.fill_circle(BADGE, 8.0, 8.0, 6.4);
            painter.outline_circle(BADGE_EDGE, 1.0, 8.0, 8.0, 6.4);
            painter.write(
                WHITE,
                10.5,
                "?",
                Rect {
                    x: 1.6,
                    y: 1.2,
                    width: 12.8,
                    height: 13.6,
                },
            );
        }
    }
}
