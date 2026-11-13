//! Script pane: the tabbed document list, file I/O, and "run this in the console".

use crate::highlight;
use crate::state::{console_write, read, set_status, write, Doc, DIM_FG};

const EDITOR_FONT: &str = "Consolas";
const EDITOR_HEIGHT: i32 = 220; // twips: 11pt

/// Applies the editor font and paints the syntax colours of the current text.
pub fn repaint_editor() {
    write(|s| {
        if s.switching_tab {
            return;
        }
        s.switching_tab = true;
        let caret = s.editor.selection();
        let len = s.editor.text().chars().count() as u32;
        s.editor.set_selection(0..len);
        s.editor.set_char_format(&nwg::CharFormat {
            font_face_name: Some(EDITOR_FONT.to_string()),
            height: Some(EDITOR_HEIGHT),
            ..Default::default()
        });
        let text = s.editor.text();
        highlight::highlight(&s.editor, &text);
        s.editor.set_selection(caret.start..caret.end);
        s.switching_tab = false;
    });
}

/// Pushes the current document's text into the editor control.
pub fn load_editor_from_doc() {
    let doc = read(|s| {
        s.docs
            .get(s.current)
            .map(|d| (d.title.clone(), d.text.clone()))
    })
    .flatten();

    let Some((title, text)) = doc else { return };
    write(|s| {
        s.switching_tab = true;
        s.editor.set_text(&text.replace("\r\n", "\n").replace('\r', "\n"));
        s.switching_tab = false;
    });
    repaint_editor();
    set_status(&title);
}

/// Snapshot of the editor contents, unless a tab switch is in progress.
pub fn editor_text() -> String {
    if read(|s| s.switching_tab).unwrap_or(false) {
        return read(|s| {
            s.docs
                .get(s.current)
                .map(|d| d.text.clone())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    }
    read(|s| s.editor.text()).unwrap_or_default()
}

/// Pulls the editor contents into the document record before leaving the tab.
fn commit_editor() {
    write(|s| {
        let text = s.editor.text();
        if let Some(doc) = s.docs.get_mut(s.current) {
            doc.text = text;
        }
    });
}

pub fn new_document() {
    commit_editor();
    write(|s| {
        let index = s.docs.len() + 1;
        s.docs.push(Doc {
            title: format!("Untitled {}", index),
            path: None,
            text: String::new(),
        });
        s.current = s.docs.len() - 1;
    });
    sync_tabs();
    load_editor_from_doc();
}

pub fn open_path(path: &std::path::Path) {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = decode(&bytes);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            commit_editor();
            write(|s| {
                let existing = s.docs.iter().position(|d| d.path.as_deref() == Some(path));
                match existing {
                    Some(found) => {
                        if let Some(doc) = s.docs.get_mut(found) {
                            doc.text = text;
                        }
                        s.current = found;
                    }
                    None => {
                        s.docs.push(Doc {
                            title: name,
                            path: Some(path.to_path_buf()),
                            text,
                        });
                        s.current = s.docs.len() - 1;
                    }
                }
            });
            sync_tabs();
            load_editor_from_doc();
        }
        Err(e) => {
            let message = format!("Could not read {}:\n{}", path.display(), e);
            match read(|s| s.window.handle) {
                Some(parent) => nwg::modal_error_message(parent, "Open script", &message),
                None => nwg::error_message("Open script", &message),
            };
        }
    }
}

/// Strips a UTF-8 or UTF-16 BOM so the first token is never invisible.
fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&bytes[3..]).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .filter_map(|b| b.get(0..2).map(|b| u16::from_le_bytes([b[0], b[1]])))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .filter_map(|b| b.get(0..2).map(|b| u16::from_be_bytes([b[0], b[1]])))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn open_dialog() {
    let mut dialog: nwg::FileDialog = Default::default();
    if nwg::FileDialog::builder()
        .title("Open PowerShell Script")
        .filters("PowerShell files (*.ps1;*.psm1;*.psd1)|All files (*.*)")
        .action(nwg::FileDialogAction::Open)
        .build(&mut dialog)
        .is_err()
    {
        set_status("Could not create the Open dialog");
        return;
    }

    let parent = read(|s| s.window.handle);
    let Some(parent) = parent else { return };
    if dialog.run(Some(parent)) {
        if let Ok(item) = dialog.get_selected_item() {
            open_path(std::path::Path::new(&item));
        }
    }
}

pub fn save_current(save_as: bool) {
    commit_editor();
    let known = if save_as {
        None
    } else {
        read(|s| s.docs.get(s.current).and_then(|d| d.path.clone())).unwrap_or(None)
    };

    match known {
        Some(path) => {
            let text = read(|s| s.editor.text()).unwrap_or_default();
            write_file(&path, &text);
        }
        None => save_as_dialog(),
    }
}

fn save_as_dialog() {
    let mut dialog: nwg::FileDialog = Default::default();
    if nwg::FileDialog::builder()
        .title("Save PowerShell Script")
        .filters("PowerShell files (*.ps1;*.psm1;*.psd1)|All files (*.*)")
        .action(nwg::FileDialogAction::Save)
        .build(&mut dialog)
        .is_err()
    {
        set_status("Could not create the Save dialog");
        return;
    }

    let parent = read(|s| s.window.handle);
    let Some(parent) = parent else { return };
    if dialog.run(Some(parent)) {
        if let Ok(item) = dialog.get_selected_item() {
            let text = read(|s| s.editor.text()).unwrap_or_default();
            write_file(std::path::Path::new(&item), &text);
        }
    }
}

fn write_file(path: &std::path::Path, text: &str) {
    let data = text.replace("\r\n", "\n").replace('\r', "\n");
    match std::fs::write(path, data.as_bytes()) {
        Ok(()) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            write(|s| {
                if let Some(doc) = s.docs.get_mut(s.current) {
                    doc.path = Some(path.to_path_buf());
                    doc.title = name.clone();
                    doc.text = text.to_string();
                }
            });
            sync_tabs();
            set_status(&format!("Saved {}", path.display()));
        }
        Err(e) => {
            let message = format!("Could not write {}:\n{}", path.display(), e);
            match read(|s| s.window.handle) {
                Some(parent) => nwg::modal_error_message(parent, "Save script", &message),
                None => nwg::error_message("Save script", &message),
            };
        }
    }
}

/// Keeps the tab strip in step with the open documents: headers are added and renamed,
/// never rebuilt. A Win32 tab control gives back no header once it has been handed one,
/// and the page windows native-windows-gui creates beside them keep their own place in
/// the strip, so a rebuild that only added new ones left the old titles behind. Closing
/// is the one place that really does delete, and `close_tab` does it itself.
pub fn sync_tabs() {
    let (titles, current) = read(|s| {
        (
            s.docs.iter().map(|d| d.title.clone()).collect::<Vec<_>>(),
            s.current,
        )
    })
    .unwrap_or((Vec::new(), 0));

    let mut error = None;
    write(|s| {
        s.switching_tab = true;
        // More pages than documents: only a delete can do that, and close_tab does it
        // itself. Truncate rather than let the two drift apart.
        if s.pages.len() > titles.len() {
            s.pages.truncate(titles.len());
            s.tab_titles.truncate(titles.len());
        }
        // A header that is already there is renamed in place; only genuinely new
        // documents get a new tab.
        for index in 0..titles.len().min(s.pages.len()) {
            let title = &titles[index];
            let changed = match s.tab_titles.get(index) {
                Some(old) => old != title,
                None => true,
            };
            if changed {
                if let Some(page) = s.pages.get(index) {
                    page.set_text(title);
                }
                if index < s.tab_titles.len() {
                    s.tab_titles[index] = title.clone();
                } else {
                    s.tab_titles.push(title.clone());
                }
            }
        }
        for title in titles.iter().skip(s.pages.len()) {
            let mut page: nwg::Tab = Default::default();
            match nwg::Tab::builder().parent(&s.tabs).text(title).build(&mut page) {
                Ok(()) => {
                    s.pages.push(page);
                    s.tab_titles.push(title.clone());
                }
                Err(e) => error = Some(e.to_string()),
            }
        }
        // The control shows a page only when it is told to, and exactly one should show.
        for (index, page) in s.pages.iter().enumerate() {
            page.set_visible(index == current);
        }
        if current < s.pages.len() {
            s.tabs.set_selected_tab(current);
        }
        s.switching_tab = false;
    });

    if let Some(e) = error {
        set_status(&format!("Could not create a script tab: {}", e));
    }
}

/// Removes a header from the strip, and with it the page window Win32 owns. The header
/// cannot come back, which is why this is the only place the strip shrinks.
fn delete_tab_header(index: usize) {
    const TCM_DELETEITEM: u32 = 0x1308;
    let Some(hwnd) = read(|s| {
        s.tabs
            .handle
            .hwnd()
            .map(|h| h as *mut std::ffi::c_void)
    })
    .flatten() else {
        return;
    };
    crate::console::send_message(hwnd, TCM_DELETEITEM, index, 0);
}

/// Moves to the document on the tab the user clicked.
pub fn select_tab(index: usize) {
    commit_editor();
    let moved = write(|s| {
        if index >= s.docs.len() {
            return false;
        }
        s.current = index;
        true
    })
    .unwrap_or(false);
    if moved {
        load_editor_from_doc();
    }
}

pub fn close_tab(index: usize) {
    let total = read(|s| s.docs.len()).unwrap_or(0);
    if total == 0 {
        return;
    }
    let index = index.min(total - 1);
    delete_tab_header(index);
    write(|s| {
        if index < s.pages.len() {
            // Dropping the page destroys its window; the header itself is already gone.
            s.pages.remove(index);
            s.tab_titles.remove(index);
        }
        s.docs.remove(index);
        if s.docs.is_empty() {
            s.docs.push(Doc {
                title: "Untitled 1".to_string(),
                path: None,
                text: String::new(),
            });
        }
        s.current = index.min(s.docs.len() - 1);
    });
    sync_tabs();
    load_editor_from_doc();
}

/// The text currently selected in the editor, if any.
pub fn editor_selection() -> String {
    let (start, end) = match read(|s| s.editor.selection()) {
        Some(range) => (range.start as usize, range.end as usize),
        None => return String::new(),
    };
    if end <= start {
        return String::new();
    }
    let text = read(|s| s.editor.text()).unwrap_or_default();
    let chars: Vec<char> = text.chars().collect();
    let a = start.min(chars.len());
    let b = end.min(chars.len());
    if b <= a {
        return String::new();
    }
    chars[a..b].iter().collect()
}

/// Sends `text` to the interactive shell as one submission, echoing it first, the way
/// pressing Enter after typing a block would.
pub fn run_text(text: &str) {
    if text.trim().is_empty() {
        set_status("Nothing to run");
        return;
    }
    let ready = read(|s| !s.busy && s.engine.is_some()).unwrap_or(false);
    if !ready {
        crate::state::beep();
        set_status("The console is still running the previous command");
        return;
    }

    // Echo first, outside the state borrow: the console writes through the same state.
    console_write(&format!("{}\n", text.trim_end()), DIM_FG);
    write(|s| {
        if let Some(engine) = s.engine.as_mut() {
            engine.execute(&text.replace("\r\n", "\n").replace('\n', "\r\n"));
        }
        s.input_open = false;
        s.busy = true;
    });
    set_status("Running script... (Ctrl+C or the Stop button to interrupt)");
}

pub fn run_script() {
    run_text(&editor_text());
}

pub fn run_selection() {
    let selection = editor_selection();
    if selection.trim().is_empty() {
        run_script();
    } else {
        run_text(&selection);
    }
}
