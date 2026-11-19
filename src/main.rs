//! PS7 ISE - an open-source PowerShell ISE that hosts PowerShell 7 in-process.
//!
//! The layout follows the original ISE: a toolbar, a tabbed script pane, the interactive
//! console underneath it, and a status bar. The console is a single rich text control
//! that shows shell output and accepts typed input after the prompt; keystrokes that
//! belong to the shell (Enter, Tab, arrows, Ctrl+C) are taken over by a window subclass
//! in [`input`], so the pane behaves like a console instead of an editor.

// A GUI application: no console window is attached, diagnostics go to the panes.
#![windows_subsystem = "windows"]

extern crate native_windows_gui as nwg;

mod commands;
mod console;
mod docs;
mod engine;
mod gutter;
mod highlight;
mod icons;
mod input;
mod panel;
mod state;
mod theme;

use console::ConsoleView;
use nwg::Event as E;
use state::{cmd, AppState};
use std::ffi::c_void;
use std::collections::HashMap;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const TITLE: &str = "Windows PowerShell 7 ISE";

const TOOLBAR_H: i32 = 30;
const TAB_STRIP_H: i32 = 24;
const GAP: i32 = 4;
const STATUS_H: i32 = 22;
const BORDER: i32 = 2;
/// Width of the line-number gutter to the left of the script pane. Four digits and the
/// separator line fit in it, which is more line than any script reaches.
const GUTTER_W: i32 = 40;
/// The toolbar button that carries the UAC shield: the 16 px icon plus the button's own
/// padding, and nothing else, because a shield needs no caption.
const SHIELD_W: i32 = 34;
/// The status bar's right-hand panel, where the caret position goes.
const CARET_W: i32 = 150;

/// Subclass ids at or below 0xFFFF belong to native-windows-gui.
const CONSOLE_SUBCLASS_ID: usize = 0x0002_0001;
const EDITOR_SUBCLASS_ID: usize = 0x0002_0002;
/// The frame, for the two messages that decide whether a dark window looks dark: the one that
/// erases its background, and the ones its child controls ask before they paint themselves.
const FRAME_SUBCLASS_ID: usize = 0x0002_0003;

fn main() {
    // Before `nwg::init`, not merely before the controls are built: initialising native-windows-gui
    // initialises comctl32, and comctl32 decides once, at that moment, whether its common controls
    // - the buttons, the tab strip, the status bar - draw light or dark. Asked later, the palette
    // still recolours everything this program paints itself and leaves the rest light, which is
    // exactly the half-dark window the first screenshot showed.
    let _ = theme::palette();

    if let Err(e) = nwg::init() {
        eprintln!("Could not start the GUI: {}", e);
        return;
    }

    let state = build_ui();
    state::set_state(state);

    // Explorer reads the icon out of the exe; the title bar and the taskbar button are
    // separate, and are given that same resource here.
    let iconed = state::read(|s| {
        s.window
            .handle
            .hwnd()
            .map(|hwnd| icons::apply(hwnd as *mut c_void))
    })
    .flatten()
    .unwrap_or(false);
    if !iconed && state::debug_enabled() {
        state::log_line("icons: the exe carries no icon resource");
    }

    bind_handlers();

    docs::sync_tabs();
    docs::load_editor_from_doc();
    open_command_line_scripts();
    layout();
    state::focus_console();

    nwg::dispatch_thread_events();
}

/// Opens the script files named on the command line, so `PS7_ISE.exe demo.ps1`, a
/// shortcut, or "Open with" behave the way they do with the original ISE. Anything that
/// is not an existing file is ignored: a stray argument must not stop the window opening.
fn open_command_line_scripts() {
    let paths: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .collect();
    if paths.is_empty() {
        return;
    }
    for path in &paths {
        docs::open_path(path);
    }
    // The startup tab is an empty "Untitled 1"; with real files open it is clutter, and
    // Ctrl+N still makes one on demand.
    let untitled = state::read(|s| {
        s.docs
            .first()
            .map(|doc| doc.path.is_none() && doc.text.is_empty())
            .unwrap_or(false)
    })
    .unwrap_or(false);
    if untitled {
        docs::close_tab(0);
    }
}

// ------------------------------------------------------------------ user interface

/// Creates every control and hands ownership to one leaked state block: native
/// event handlers are `'static`, and the process owns the controls for its lifetime.
#[allow(deprecated)] // nwg::Timer is the win32 SetTimer hook: it fires on this thread.
fn build_ui() -> *mut AppState {
    // Before anything is created: the palette reads the user's app mode, and in dark mode it tells
    // comctl32 to draw its controls dark. That switch is read when a control is created, so a
    // window told after the fact keeps light buttons and a light tab strip - which is the half-dark
    // window this line exists to prevent.
    let palette = theme::palette();

    let mut window: nwg::Window = Default::default();
    nwg::Window::builder()
        .size((1120, 780))
        .position((90, 60))
        .title(&format!("{} - v{}", TITLE, VERSION))
        .flags(nwg::WindowFlags::MAIN_WINDOW | nwg::WindowFlags::VISIBLE)
        .build(&mut window)
        .expect("Could not create the main window");

    // The title bar is the desktop window manager's, not this program's, and it has its own
    // opinion about light and dark unless it is told.
    if let Some(hwnd) = window.handle.hwnd() {
        theme::apply_titlebar(hwnd as *mut c_void);
    }

    let menus = build_menus(&window);

    let mut tabs: nwg::TabsContainer = Default::default();
    nwg::TabsContainer::builder()
        .parent(&window)
        .position((BORDER, TOOLBAR_H))
        .size((1116, TAB_STRIP_H))
        .build(&mut tabs)
        .expect("Could not create the script tab strip");

    let mut editor: nwg::RichTextBox = Default::default();
    nwg::RichTextBox::builder()
        .parent(&window)
        .flags(
            nwg::RichTextBoxFlags::VSCROLL
                | nwg::RichTextBoxFlags::AUTOVSCROLL
                | nwg::RichTextBoxFlags::HSCROLL
                | nwg::RichTextBoxFlags::AUTOHSCROLL,
        )
        .build(&mut editor)
        .expect("Could not create the script pane");

    // A narrow label to the left of the script pane that shows the line number of the row
    // the caret is on, like the edit window of the original ISE. A rich edit has no built-in
    // gutter, and painting one with WM_PAINT would fight its own paint messages, so the
    // number is repainted from the editor's selection whenever it moves.
    let mut console: nwg::RichTextBox = Default::default();
    nwg::RichTextBox::builder()
        .parent(&window)
        .flags(
            nwg::RichTextBoxFlags::VSCROLL
                | nwg::RichTextBoxFlags::AUTOVSCROLL
                | nwg::RichTextBoxFlags::HSCROLL
                | nwg::RichTextBoxFlags::AUTOHSCROLL,
        )
        .build(&mut console)
        .expect("Could not create the console pane");

    let mut status: nwg::StatusBar = Default::default();
    nwg::StatusBar::builder()
        .parent(&window)
        .text("Starting PowerShell 7...")
        .build(&mut status)
        .expect("Could not create the status bar");

    let mut font: nwg::Font = Default::default();
    if nwg::Font::builder()
        .family("Consolas")
        .size(11)
        .build(&mut font)
        .is_err()
    {
        nwg::Font::builder()
            .family("Courier New")
            .size(11)
            .build(&mut font)
            .ok();
    }
    editor.set_font(Some(&font));
    console.set_font(Some(&font));

    // native-windows-gui leaves the rich edit text limit at 0, which makes the control
    // refuse every EM_REPLACESEL insertion without reporting anything: the console would
    // stay blank and the script pane would swallow typing. A console session grows past
    // 32 767 characters (the classic edit limit) almost immediately, so both panes get a
    // generous one.
    const TEXT_LIMIT: usize = 16 * 1024 * 1024;
    editor.set_limit(TEXT_LIMIT);
    console.set_limit(TEXT_LIMIT);

    // Line numbers down the left of the script pane, like the edit window of the original
    // ISE. A rich edit has no gutter of its own, so this is a window of `gutter`'s own class
    // that draws them from the editor's rows, which keeps each number beside its line even
    // when a long one is wrapped. The font is smaller than the script's, the way a margin
    // reads; the control is given it with WM_SETFONT and keeps using it.
    let mut gutter_font: nwg::Font = Default::default();
    if nwg::Font::builder()
        .family("Consolas")
        .size(9)
        .build(&mut gutter_font)
        .is_err()
    {
        nwg::Font::builder()
            .family("Courier New")
            .size(9)
            .build(&mut gutter_font)
            .ok();
    }
    let parent = window.handle.hwnd().map(|h| h as *mut c_void);
    let gutter = parent.and_then(|parent| gutter::create(parent, gutter_font.handle as *mut c_void));
    if gutter.is_none() {
        state::set_status("Could not create the line-number gutter");
    }

    // The Commands column down the right-hand side, like the original ISE's. It gets a font of its
    // own because the panes' is bigger still, and the column has to fit a parameter row
    // (`[x] Verbose <SwitchParameter>`) and a long command name in a narrow strip.
    //
    // `size` here is not points: native-windows-gui passes it straight to `CreateFontW` as a
    // positive cell height in device units, so 12 is a cell about 12 units tall - around 9 points
    // at 96 dpi - and the 9 this used to be was nearer 7, which is what made the list hard to
    // read. The column now takes a third of the window's width rather than a fixed 322 pixels,
    // so the extra two points of width the larger text costs has somewhere to come from.
    let mut panel_font: nwg::Font = Default::default();
    if nwg::Font::builder()
        .family("Consolas")
        .size(12)
        .build(&mut panel_font)
        .is_err()
    {
        nwg::Font::builder()
            .family("Courier New")
            .size(12)
            .build(&mut panel_font)
            .ok();
    }
    let commands = panel::create(&window, Some(&panel_font));

    // Shell output is polled on this timer, which also starts the engine once the
    // window has been shown.
    let mut timer: nwg::Timer = Default::default();
    nwg::Timer::builder()
        .interval(40)
        .parent(&window)
        .build(&mut timer)
        .expect("Could not create the update timer");
    timer.start();

    let buttons = build_toolbar(&window);

    let console_hwnd = console.handle.hwnd().map(|h| h as *mut c_void);
    if let Some(hwnd) = console_hwnd {
        let view = ConsoleView::new(hwnd);
        view.set_background(state::CONSOLE_BG);
    }
    // The script pane's field is the other rich edit, and until now it never had one set: it took
    // the control's own white, which is what made a dark window have a bright rectangle in the
    // middle of it. The console deliberately keeps its terminal background in both modes, for the
    // reason in `theme`'s header comment.
    if let Some(hwnd) = editor.handle.hwnd().map(|h| h as *mut c_void) {
        ConsoleView::new(hwnd).set_background(palette.editor_bg);
    }

    let state = AppState {
        window,
        tabs,
        pages: Vec::new(),
        tab_titles: Vec::new(),
        editor,
        console,
        status,
        console_view: console_hwnd.map(ConsoleView::new),
        line_gutter: gutter,
        gutter_font: Some(gutter_font),
        caret_text: String::new(),
        gutter_key: (0, 1, 0),
        menus: menus.0,
        menu_items: menus.1,
        items: menus.2,
        buttons: buttons.0,
        button_items: buttons.1,
        _font: Some(font),
        broken_modules: HashMap::new(),
        timer: Some(timer),
        engine_pending: true,
        split: 0.5,
        show_script: true,
        show_console: true,
        show_commands: true,
        commands,
        panel_font: Some(panel_font),
        engine: None,
        docs: vec![state::Doc {
            title: "Untitled 1".to_string(),
            path: None,
            text: String::new(),
        }],
        current: 0,
        switching_tab: false,
        location: String::new(),
        prompt_start: 0,
        console_buffer: String::new(),
        console_runs: Vec::new(),
        draft_caret: 0,
        input_open: false,
        busy: false,
        history: Vec::new(),
        history_pos: 0,
        draft: String::new(),
        candidates: Vec::new(),
        candidate_pos: 0,
        last_completion: None,
    };

    Box::into_raw(Box::new(state))
}

/// Menus mirror the original ISE. The `\t...` part only labels the item: keyboard
/// shortcuts are matched in `on_key_press`, because these menus cannot own accelerators.
/// The items are returned as well, since dropping a `MenuItem` removes it from the menu.
fn build_menus(
    window: &nwg::Window,
) -> (
    Vec<nwg::Menu>,
    Vec<nwg::MenuItem>,
    Vec<(cmd::Id, nwg::ControlHandle)>,
) {
    let mut menus: Vec<nwg::Menu> = Vec::new();
    let mut kept: Vec<nwg::MenuItem> = Vec::new();
    let mut items: Vec<(cmd::Id, nwg::ControlHandle)> = Vec::new();

    for text in ["&File", "&Edit", "&Debug", "&View", "&Help"] {
        let mut menu: nwg::Menu = Default::default();
        if nwg::Menu::builder()
            .text(text)
            .parent(window)
            .build(&mut menu)
            .is_err()
        {
            state::set_status(&format!("Could not create the {} menu", text));
        }
        menus.push(menu);
    }

    let mut add = |menu: usize, text: &str, id: cmd::Id| {
        let mut item: nwg::MenuItem = Default::default();
        match nwg::MenuItem::builder()
            .text(text)
            .parent(&menus[menu])
            .build(&mut item)
        {
            Ok(()) => {
                items.push((id, item.handle));
                kept.push(item);
            }
            Err(e) => state::set_status(&format!("Could not add {} ({})", text, e)),
        }
    };

    add(0, "&New Script\tCtrl+N", cmd::NEW);
    add(0, "&Open Script...\tCtrl+O", cmd::OPEN);
    add(0, "&Save Script\tCtrl+S", cmd::SAVE);
    add(0, "Save Script &As...\tCtrl+Shift+S", cmd::SAVE_AS);
    add(0, "&Close Script\tCtrl+W", cmd::CLOSE_TAB);
    add(0, "E&xit", cmd::EXIT);

    add(1, "Cu&t", cmd::CUT);
    add(1, "&Copy", cmd::COPY);
    add(1, "&Paste", cmd::PASTE);

    add(2, "&Run Script\tF5", cmd::RUN_SCRIPT);
    add(2, "Run &Selection\tF8", cmd::RUN_SELECTION);
    add(2, "&Stop Run\tCtrl+C", cmd::STOP);

    add(3, "Show/Hide &Script Pane", cmd::TOGGLE_SCRIPT_PANE);
    add(3, "Show/Hide &Command Pane", cmd::TOGGLE_CONSOLE_PANE);
    add(3, "C&lear Command Pane", cmd::CLEAR_CONSOLE);
    // "Commands", plural, and a different verb: the item above called the console the command
    // pane, and two "Show/Hide Command..." lines in one menu would be unreadable.
    add(3, "Sho&w Commands Column", cmd::TOGGLE_COMMANDS);
    add(3, "&Refresh Command List", cmd::REFRESH_COMMANDS);

    add(4, "PowerShell 7 &Help\tF1", cmd::HELP);
    add(4, "&About PowerShell ISE", cmd::ABOUT);

    (menus, kept, items)
}

/// The ISE toolbar is a row of buttons; Win32 toolbars need an image list, and text
/// buttons keep the clone dependency-free.
fn build_toolbar(window: &nwg::Window) -> (Vec<nwg::Button>, Vec<(cmd::Id, nwg::ControlHandle)>) {
    const LABELS: &[(cmd::Id, &str, i32)] = &[
        (cmd::NEW, "New", 60),
        (cmd::OPEN, "Open", 60),
        (cmd::SAVE, "Save", 60),
        (cmd::RUN_SCRIPT, "Run Script", 90),
        (cmd::RUN_SELECTION, "Run Selection", 100),
        (cmd::STOP, "Stop", 60),
        (cmd::CLEAR_CONSOLE, "Clear Pane", 80),
        (cmd::HELP, "Help", 60),
    ];

    let mut buttons = Vec::new();
    let mut handles = Vec::new();
    let mut x = BORDER;
    for (id, label, width) in LABELS {
        let mut button: nwg::Button = Default::default();
        match nwg::Button::builder()
            .text(label)
            .parent(window)
            .position((x, 1))
            .size((*width, TOOLBAR_H - 4))
            .build(&mut button)
        {
            Ok(()) => {
                handles.push((*id, button.handle));
                x += width + 3;
                buttons.push(button);
            }
            Err(e) => state::set_status(&format!("Could not create the {} button ({})", label, e)),
        }
    }

    // Relaunching elevated needs no words: the UAC shield on a button is the one control
    // every Windows user reads correctly, and it does not have to be translated either. The
    // shell draws the shield, and a tooltip says what the click will ask for.
    let mut shield: nwg::Button = Default::default();
    let builder = nwg::Button::builder()
        .text("")
        .parent(window)
        .position((x, 1))
        .size((SHIELD_W, TOOLBAR_H - 4))
        .flags(nwg::ButtonFlags::VISIBLE | nwg::ButtonFlags::ICON | nwg::ButtonFlags::TAB_STOP);
    match builder.build(&mut shield) {
        Ok(()) => {
            let hwnd = shield.handle.hwnd().map(|h| h as *mut c_void);
            if !hwnd.map(icons::set_shield).unwrap_or(false) {
                // No shield to show, so the button has to say it in words instead.
                shield.set_text("Run as Admin");
            }
            let mut tip: nwg::Tooltip = Default::default();
            if nwg::Tooltip::builder()
                .register(&shield.handle, "Run as Administrator")
                .build(&mut tip)
                .is_ok()
            {
                // Owned by the tooltip window the builder creates; it lives as long as the
                // button it is attached to, which is as long as the process.
                std::mem::forget(tip);
            }
            handles.push((cmd::RUN_AS_ADMIN, shield.handle));
            buttons.push(shield);
        }
        Err(e) => state::set_status(&format!(
            "Could not create the Administrator button ({})",
            e
        )),
    }

    (buttons, handles)
}

/// Places the panes for the current window size and split.
fn layout() {
    state::read(|s| {
        let Some(hwnd) = s.window.handle.hwnd() else {
            return;
        };
        let (width, height) = console::client_size(hwnd as *mut c_void);
        // The Commands column takes its width off the right, and both panes give it up: the
        // original ISE docks it the same way rather than floating it over the panes. The width is
        // whatever the window can spare rather than a fixed number, so a wider window widens the
        // column along with the panes instead of leaving a strip of the old size beside them.
        let panel_width = if s.show_commands {
            panel::width_for(width) + GAP
        } else {
            0
        };
        let tabs_width = (width - 2 * BORDER - panel_width).max(10) as u32;
        s.tabs.set_position(BORDER, TOOLBAR_H);
        s.tabs.set_size(tabs_width, TAB_STRIP_H as u32);

        let pane_top = TOOLBAR_H + TAB_STRIP_H + 2;
        let usable = (height - pane_top - STATUS_H - GAP).max(40);
        let script_h = if s.show_script && s.show_console {
            (usable as f32 * s.split).round() as i32
        } else if s.show_script {
            usable
        } else {
            0
        };

        s.editor.set_visible(s.show_script);
        s.tabs.set_visible(s.show_script);
        s.console.set_visible(s.show_console);

        if s.show_script {
            let editor_left = BORDER + GUTTER_W + 2;
            let pane_h = (script_h - TAB_STRIP_H).max(20);
            s.editor.set_position(editor_left, pane_top + TAB_STRIP_H);
            s.editor.set_size(
                ((tabs_width as i32) - GUTTER_W - 2).max(20) as u32,
                pane_h as u32,
            );
            if let Some(strip) = s.line_gutter {
                // The strip shares the editor's top edge, so a number drawn at the row the
                // editor reports lands beside the text of that row.
                gutter::set_geometry(strip, BORDER, pane_top + TAB_STRIP_H, GUTTER_W, pane_h);
                gutter::set_visible(strip, true);
            }
        } else if let Some(strip) = s.line_gutter {
            gutter::set_visible(strip, false);
        }
        if s.show_console {
            let top = pane_top + script_h + GAP;
            s.console.set_position(BORDER, top);
            s.console.set_size(tabs_width, (height - top - STATUS_H - 2).max(40) as u32);
        }

        // A status bar has exactly one panel unless it is told otherwise, and the caret
        // readout needs a second one at the right-hand end.
        if let Some(bar) = s.status.handle.hwnd() {
            console::set_status_parts(
                bar as *mut c_void,
                &[(width - CARET_W - BORDER).max(10), -1],
            );
        }
    });

    // The column itself is placed through a write: laying it out means taking the controls
    // mutably, which the read above cannot hand out.
    state::write(|s| {
        let Some(hwnd) = s.window.handle.hwnd() else {
            return;
        };
        let (width, height) = console::client_size(hwnd as *mut c_void);
        // The same width the panes above gave up, from the same client width: if the two
        // calculations disagreed the column would be laid out at one size while the panes
        // reserved another, and the seam between them would move on every resize.
        let column_w = panel::width_for(width);
        let x = (width - BORDER - column_w).max(BORDER);
        let height = (height - TOOLBAR_H - STATUS_H - GAP).max(120);
        panel::layout(&mut s.commands, x, TOOLBAR_H, column_w, height, s.show_commands);
    });
}

// ------------------------------------------------------------------ event handling

/// Hooks every control created by [`build_ui`] into one dispatcher, and lets the console
/// pane take the keys that belong to the shell.
fn bind_handlers() {
    let Some(window) = state::read(|s| s.window.handle) else {
        return;
    };

    // Menus, toolbar buttons, tabs, keys, resize and timer ticks of the window and all
    // of its children are routed through `dispatch`.
    nwg::full_bind_event_handler(&window, move |evt, data, handle| {
        dispatch(evt, data, handle)
    });

    // The frame paints its own background, and answers for every child control that asks: the
    // list boxes, the edits, the labels. Those questions arrive at the parent, so one hook on the
    // window colours the whole column and the toolbar in one place. In light mode the palette says
    // nothing about either message, which leaves Windows to do what it has always done.
    if let Ok(handler) =
        nwg::bind_raw_event_handler(&window, FRAME_SUBCLASS_ID, move |hwnd, msg, w, _l| {
            let palette = theme::palette();
            let dc = w as *mut c_void;
            match msg {
                theme::WM_ERASEBKGND => palette.erase_background(hwnd as *mut c_void, dc),
                theme::WM_CTLCOLORSTATIC
                | theme::WM_CTLCOLOREDIT
                | theme::WM_CTLCOLORLISTBOX
                | theme::WM_CTLCOLORDLG => palette.child_colour(msg, dc),
                _ => None,
            }
        })
    {
        std::mem::forget(handler);
    }

    // Bound last on purpose: the subclass chain calls the newest hook first, so the
    // console can swallow a key before the rich text control or nwg ever sees it.
    let Some(console) = state::read(|s| s.console.handle) else {
        return;
    };
    if let Ok(handler) =
        nwg::bind_raw_event_handler(&console, CONSOLE_SUBCLASS_ID, move |_hwnd, msg, w, _l| {
            match msg {
                console::WM_CHAR => match char::from_u32(w as u32) {
                    Some(ch) if input::on_char(ch) => Some(0),
                    _ => None,
                },
                console::WM_KEYDOWN => {
                    // Shortcuts first: the console must answer F5 and Ctrl+S too, and it
                    // is the one place where a keystroke demonstrably arrives.
                    if shortcut(w as u32, false) || input::on_keydown(w as u32) {
                        Some(0)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        })
    {
        // The hook has to outlive this function; it goes away with the process.
        std::mem::forget(handler);
    }

    // The same shortcuts are wanted while editing, so the script pane gets its own hook.
    // Unlike the console it passes on every key it is not asked for, and it leaves
    // Ctrl+C alone so the control can copy the selection.
    let Some(editor) = state::read(|s| s.editor.handle) else {
        return;
    };
    if let Ok(handler) =
        nwg::bind_raw_event_handler(&editor, EDITOR_SUBCLASS_ID, move |_hwnd, msg, w, _l| {
            if msg == console::WM_KEYDOWN && shortcut(w as u32, true) {
                return Some(0);
            }
            None
        })
    {
        std::mem::forget(handler);
    }
}

/// Every command and layout message from the window and its children. Keys are handled by
/// the two subclass hooks, so no key events arrive here.
fn dispatch(evt: E, _data: nwg::EventData, handle: nwg::ControlHandle) {
    match evt {
        E::OnWindowClose => {
            if state::is(&handle, |s| s.window.handle) {
                nwg::stop_thread_dispatch();
            }
        }
        E::OnResize => {
            if state::is(&handle, |s| s.window.handle) {
                layout();
            }
        }
        E::OnTimerTick => on_tick(),
        E::OnMenuItemSelected | E::OnButtonClick => {
            // A click from a control that is not in the menu table came out of the Commands
            // column: its buttons and its check box are not menu commands.
            match state::command_for(&handle) {
                Some(id) => run_command(id),
                None => {
                    panel::on_event(evt, &handle);
                }
            }
        }
        // The Commands column's dropdown and its two lists.
        E::OnComboxBoxSelection | E::OnListBoxSelect | E::OnListBoxDoubleClick => {
            panel::on_event(evt, &handle);
        }
        E::TabsContainerChanged => {
            if state::is(&handle, |s| s.tabs.handle) {
                let index = state::read(|s| s.tabs.selected_tab()).unwrap_or(0);
                docs::select_tab(index);
            }
        }
        E::OnTextInput => {
            if state::is(&handle, |s| s.editor.handle) {
                docs::repaint_editor();
            }
            // The column's Find box and its parameter value box are edit controls too, and
            // both filter or remember as they are typed.
            panel::on_event(evt, &handle);
        }
        _ => {}
    }
}

fn on_tick() {
    if state::read(|s| s.engine_pending).unwrap_or(false) {
        start_engine();
        return;
    }
    state::pump_engine();

    // The caret readout and the line numbers both follow the script pane, so they are
    // refreshed on the same tick that polls the shell. Each writes only when it has moved,
    // which keeps the status bar from being rewritten twenty five times a second.
    if let Some(pos) = state::update_caret_display() {
        state::set_status_caret(&pos);
    }
    let panes = state::read(|s| {
        let strip = *s.line_gutter.as_ref()?;
        let editor = s.editor.handle.hwnd().map(|h| h as *mut c_void)?;
        Some((strip, editor))
    });
    if let Some((strip, editor)) = panes.flatten() {
        gutter::refresh(strip, editor);
    }
}

/// Keyboard shortcuts. These menus cannot own Win32 accelerators, so the key messages the
/// two panes report are matched here instead, from their subclass hooks - the dispatch
/// level proved unreliable for the modified combinations. Returns true when the key was
/// acted on, which stops it reaching the control underneath.
fn shortcut(vk: u32, from_editor: bool) -> bool {
    const VK_C: u32 = 0x43;
    const VK_N: u32 = 0x4E;
    const VK_O: u32 = 0x4F;
    const VK_S: u32 = 0x53;
    const VK_W: u32 = 0x57;

    let ctrl = console::ctrl_pressed();
    if !ctrl {
        match vk {
            nwg::keys::F5 => docs::run_script(),
            nwg::keys::F8 => docs::run_selection(),
            nwg::keys::F1 => show_help(),
            _ => return false,
        }
        return true;
    }

    // In the script pane Ctrl+C belongs to the control, so that Ctrl+C copies the
    // selection there. In the console it interrupts, which the console's own handler does.
    if from_editor && vk == VK_C {
        return false;
    }

    match vk {
        VK_S => docs::save_current(console::shift_pressed()),
        VK_N => docs::new_document(),
        VK_O => docs::open_dialog(),
        VK_W => {
            let current = state::read(|s| s.current).unwrap_or(0);
            docs::close_tab(current);
        }
        _ => return false,
    }
    true
}

// ------------------------------------------------------------------ commands

/// Runs a menu or toolbar command by id. The debug hook reaches it through
/// `{MENU:n}` so a test can press a menu item without a mouse.
pub fn invoke_command(id: u32) {
    run_command(id);
}

fn run_command(id: u32) {
    use cmd::*;
    match id {
        NEW => docs::new_document(),
        OPEN => docs::open_dialog(),
        SAVE => docs::save_current(false),
        SAVE_AS => docs::save_current(true),
        CLOSE_TAB => {
            let current = state::read(|s| s.current).unwrap_or(0);
            docs::close_tab(current);
        }
        EXIT => nwg::stop_thread_dispatch(),
        CUT => edit_command(console::WM_CUT),
        COPY => edit_command(console::WM_COPY),
        PASTE => edit_command(console::WM_PASTE),
        RUN_SCRIPT => docs::run_script(),
        RUN_SELECTION => docs::run_selection(),
        STOP => input::stop_execution(),
        CLEAR_CONSOLE => input::clear_console(),
        TOGGLE_SCRIPT_PANE => toggle_panes(true),
        TOGGLE_CONSOLE_PANE => toggle_panes(false),
        TOGGLE_COMMANDS => panel::toggle(),
        REFRESH_COMMANDS => panel::refresh(),
        HELP => show_help(),
        ABOUT => show_about(),
        RUN_AS_ADMIN => run_as_admin(),
        _ => {}
    }
}

/// Sends Cut/Copy/Paste to the script pane.
fn edit_command(which: u32) {
    let hwnd = state::read(|s| s.editor.handle.hwnd().map(|h| h as *mut c_void)).flatten();
    if let Some(hwnd) = hwnd {
        console::send_message(hwnd, which, 0, 0);
    }
}

/// Shows one pane or the other; the ISE can work with either alone.
fn toggle_panes(script: bool) {
    state::write(|s| {
        if script {
            s.show_script = !s.show_script;
            if !s.show_script {
                s.show_console = true;
            }
        } else {
            s.show_console = !s.show_console;
            if !s.show_console {
                s.show_script = true;
            }
        }
    });
    layout();
    state::focus_console();
}

fn show_about() {
    let text = format!(
        "Windows PowerShell 7 ISE, version {}\n\n\
         An open-source clone of the PowerShell Integrated Scripting Environment.\n\
         PowerShell 7 runs in PowerShellEngineHost.exe next to this program, so the\n\
         console, the tabbed script editor and TabExpansion2 completion talk to a\n\
         real runspace. The Windows controls are native; no web view is involved.\n\n\
         Support Me on Patreon: https://patreon.com/haplopeart",
        VERSION
    );
    match state::read(|s| s.window.handle) {
        Some(parent) => nwg::modal_info_message(parent, "About Windows PowerShell 7 ISE", &text),
        None => nwg::simple_message("About", &text),
    };
}

/// Relaunches this process elevated, which is what DISM and `MpCmdRun.exe` need and what
/// the shield on a toolbar button means. The UAC prompt is Windows' own: `ShellExecuteW`
/// with the `runas` verb is the call Explorer makes when you click "Run as administrator".
///
/// This instance is only closed once the new one is on its way. `SE_ERR_ACCESSDENIED` is
/// what the shell gives back when the prompt is dismissed, and a declined prompt is not a
/// reason to throw away the script someone has open.
fn run_as_admin() {
    const SW_SHOWNORMAL: i32 = 1;
    /// The shell reports failure as a small positive number; anything above 32 is a handle.
    const SHELL_SUCCESS: isize = 32;
    /// `SE_ERR_ACCESSDENIED`, which is what a dismissed prompt returns.
    const DECLINED: isize = 5;

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            state::set_status(&format!("Could not locate this executable: {}", e));
            return;
        }
    };

    // The new instance is asked to open the same scripts this one was started with. Each
    // argument is quoted as the shell wants it, or a path with a space in it becomes two.
    let params: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| {
            let arg = arg.to_string_lossy().replace('"', "\\\"");
            format!("\"{}\"", arg)
        })
        .collect();

    let verb = wide("runas");
    let file = wide(&exe.display().to_string());
    let parameters = wide(&params.join(" "));

    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            if parameters.len() > 1 {
                parameters.as_ptr()
            } else {
                std::ptr::null()
            },
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    if (result as isize) > SHELL_SUCCESS {
        state::set_status("Started as Administrator");
        nwg::stop_thread_dispatch();
    } else if (result as isize) == DECLINED {
        state::set_status("Elevation was declined; running without Administrator rights");
    } else {
        state::set_status(&format!(
            "Could not request elevation (shell error {})",
            result as isize
        ));
    }
}

/// A string as the shell wants it: UTF-16 with a terminating zero.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[link(name = "shell32")]
extern "system" {
    /// Returns a window handle on success and one of the `SE_ERR_*` codes below 32 on
    /// failure, which is why it is read as an integer here.
    fn ShellExecuteW(
        parent: *mut c_void,
        verb: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> *mut c_void;
}

/// F1: asks the running shell for help on the command the cursor is on.
fn show_help() {
    match editor_word_at_caret() {
        Some(word) => docs::run_text(&format!("Get-Help {} -Full", word)),
        None => state::set_status("Place the cursor on a command name, then press F1"),
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '\\'
}

/// The command name under the script pane's caret, if there is one.
fn editor_word_at_caret() -> Option<String> {
    let text = state::read(|s| s.editor.text())?;
    let caret = state::read(|s| s.editor.selection().start)? as usize;
    let chars: Vec<char> = text.chars().collect();
    if caret > chars.len() {
        return None;
    }

    let mut start = caret;
    while start > 0 && is_word_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = caret;
    while end < chars.len() && is_word_char(chars[end]) {
        end += 1;
    }

    let word: String = chars[start..end].iter().collect();
    let starts_with_letter = word.chars().next().map(|c| c.is_alphabetic()) == Some(true);
    if word.len() > 1 && word.contains('-') && starts_with_letter {
        Some(word)
    } else {
        None
    }
}

// ------------------------------------------------------------------ engine startup

/// Loads the CLR and starts the PowerShell session. Called from the timer so the window
/// is already on screen while the engine warms up.
fn start_engine() {
    state::write(|s| s.engine_pending = false);

    let Some(directory) = engine_directory() else {
        engine_failed(&format!(
            "Could not find {} next to {}.",
            engine::ENGINE_EXE,
            std::env::current_exe()
                .map(|exe| exe.display().to_string())
                .unwrap_or_else(|_| "PS7_ISE.exe".to_string())
        ));
        return;
    };

    state::set_status("Starting PowerShell 7...");
    state::log_line(&format!("[{}] starting engine from {}", std::process::id(), directory.display()));
    match engine::Engine::load(&directory) {
        Ok(mut hosted) => match hosted.start() {
            Ok(()) => {
                state::write(|s| s.engine = Some(hosted));
                state::set_status("Ready");
                state::focus_console();
                state::log_line("engine started");
            }
            Err(e) => engine_failed(&format!("PowerShell could not start:\n{}", e)),
        },
        Err(e) => engine_failed(&e),
    }
}

/// Where the published engine lives. `build.ps1` publishes it into `dist`, so it is either
/// next to `PS7_ISE.exe` or one `dist` folder away from a front-end started out of the
/// build tree.
fn engine_directory() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?.to_path_buf();
    let mut candidates: Vec<PathBuf> = vec![exe_dir.clone()];
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("dist"));
    }

    candidates.into_iter().find(|dir| {
        dir.join(engine::ENGINE_EXE).is_file()
            && dir
                .join("PowerShellEngineHost.runtimeconfig.json")
                .is_file()
    })
}

fn engine_failed(message: &str) {
    state::log_line(&format!("engine failed: {}", message));
    state::console_write(&format!("{}\n", message), state::ERROR_FG);
    state::console_write(
        "The console needs the published engine (build.ps1 copies it into dist) next to \
         PS7_ISE.exe, together with the .NET runtime that PowerShell release targets \
         (.NET 10 for PowerShell 7.6).\n",
        state::DIM_FG,
    );
    state::set_status("PowerShell engine unavailable");
}
