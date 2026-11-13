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

mod console;
mod docs;
mod engine;
mod highlight;
mod input;
mod state;

use console::ConsoleView;
use nwg::Event as E;
use state::{cmd, AppState};
use std::ffi::c_void;
use std::path::PathBuf;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const TITLE: &str = "Windows PowerShell 7 ISE";

const TOOLBAR_H: i32 = 30;
const TAB_STRIP_H: i32 = 24;
const GAP: i32 = 4;
const STATUS_H: i32 = 22;
const BORDER: i32 = 2;

/// Subclass ids at or below 0xFFFF belong to native-windows-gui.
const CONSOLE_SUBCLASS_ID: usize = 0x0002_0001;
const EDITOR_SUBCLASS_ID: usize = 0x0002_0002;

fn main() {
    if let Err(e) = nwg::init() {
        eprintln!("Could not start the GUI: {}", e);
        return;
    }

    let state = build_ui();
    state::set_state(state);

    bind_handlers();

    docs::sync_tabs();
    docs::load_editor_from_doc();
    layout();
    state::focus_console();

    nwg::dispatch_thread_events();
}

// ------------------------------------------------------------------ user interface

/// Creates every control and hands ownership to one leaked state block: native
/// event handlers are `'static`, and the process owns the controls for its lifetime.
#[allow(deprecated)] // nwg::Timer is the win32 SetTimer hook: it fires on this thread.
fn build_ui() -> *mut AppState {
    let mut window: nwg::Window = Default::default();
    nwg::Window::builder()
        .size((1120, 780))
        .position((90, 60))
        .title(&format!("{} - v{}", TITLE, VERSION))
        .flags(nwg::WindowFlags::MAIN_WINDOW | nwg::WindowFlags::VISIBLE)
        .build(&mut window)
        .expect("Could not create the main window");

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

    let state = AppState {
        window,
        tabs,
        pages: Vec::new(),
        editor,
        console,
        status,
        console_view: console_hwnd.map(ConsoleView::new),
        menus: menus.0,
        menu_items: menus.1,
        items: menus.2,
        buttons: buttons.0,
        button_items: buttons.1,
        _font: Some(font),
        timer: Some(timer),
        engine_pending: true,
        split: 0.5,
        show_script: true,
        show_console: true,
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
    (buttons, handles)
}

/// Places the panes for the current window size and split.
fn layout() {
    state::read(|s| {
        let Some(hwnd) = s.window.handle.hwnd() else {
            return;
        };
        let (width, height) = console::client_size(hwnd as *mut c_void);
        let tabs_width = (width - 2 * BORDER).max(10) as u32;
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
        let _console_h = (usable - script_h).max(0);

        s.editor.set_visible(s.show_script);
        s.tabs.set_visible(s.show_script);
        s.console.set_visible(s.show_console);

        if s.show_script {
            s.editor.set_position(BORDER, pane_top + TAB_STRIP_H);
            s.editor.set_size(tabs_width, (script_h - TAB_STRIP_H).max(20) as u32);
        }
        if s.show_console {
            let top = pane_top + script_h + GAP;
            s.console.set_position(BORDER, top);
            s.console.set_size(tabs_width, (height - top - STATUS_H - 2).max(40) as u32);
        }
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
                    if input::on_keydown(w as u32) {
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

    // F5, F8, F1 and the Ctrl shortcuts are pressed while editing, so the script pane
    // needs the same hook. Unlike the console it keeps every key it is not asked for.
    let Some(editor) = state::read(|s| s.editor.handle) else {
        return;
    };
    if let Ok(handler) =
        nwg::bind_raw_event_handler(&editor, EDITOR_SUBCLASS_ID, move |_hwnd, msg, w, _l| {
            if msg == console::WM_KEYDOWN && on_key_press(w as u32) {
                return Some(0);
            }
            None
        })
    {
        std::mem::forget(handler);
    }
}

fn dispatch(evt: E, data: nwg::EventData, handle: nwg::ControlHandle) {
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
            if let Some(id) = state::command_for(&handle) {
                run_command(id);
            }
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
        }
        E::OnKeyPress => {
            on_key_press(data.on_key());
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
}

/// Keyboard shortcuts. These menus cannot own Win32 accelerators, so the key messages
/// the controls report are matched here instead. Returns true when the key was acted on,
/// which lets the caller stop it from reaching the control underneath.
fn on_key_press(vk: u32) -> bool {
    const VK_N: u32 = 0x4E;
    const VK_O: u32 = 0x4F;
    const VK_S: u32 = 0x53;
    const VK_W: u32 = 0x57;

    let ctrl = console::ctrl_pressed();
    match (vk, ctrl) {
        (nwg::keys::F5, false) => docs::run_script(),
        (nwg::keys::F8, false) => docs::run_selection(),
        (nwg::keys::F1, false) => show_help(),
        (VK_S, true) => docs::save_current(console::shift_pressed()),
        (VK_N, true) => docs::new_document(),
        (VK_O, true) => docs::open_dialog(),
        (VK_W, true) => {
            let current = state::read(|s| s.current).unwrap_or(0);
            docs::close_tab(current);
        }
        _ => return false,
    }
    true
}

// ------------------------------------------------------------------ commands

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
        HELP => show_help(),
        ABOUT => show_about(),
        _ => {}
    }
}

/// Sends Cut/Copy/Paste to the script pane.
fn edit_command(which: u32) {
    let hwnd = state::read(|s| s.editor.handle.hwnd().map(|h| h as *mut c_void)).flatten();
    if let Some(hwnd) = hwnd {
        console::send_command(hwnd, which);
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
         PowerShell 7 is hosted in this process through hostfxr: the interactive\n\
         console, the tabbed script editor and TabExpansion2 completion are all\n\
         built on native Windows controls.",
        VERSION
    );
    match state::read(|s| s.window.handle) {
        Some(parent) => nwg::modal_info_message(parent, "About Windows PowerShell 7 ISE", &text),
        None => nwg::simple_message("About", &text),
    };
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
