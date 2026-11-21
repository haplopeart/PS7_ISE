//! The Commands panel: the right-hand column of the original ISE, which lists the commands
//! the session can see and builds a command line out of the ones the user picks.
//!
//! The wire format, the answers and what they mean are in [`crate::commands`]; this file is
//! only the window: the Module dropdown, the Find box, the command list, the syntax area, the
//! parameter list, and the Insert / Run / Copy buttons along the bottom.
//!
//! The controls are stock Windows controls parented to the main window, so the frame routes
//! their notifications through `dispatch` exactly as it does the toolbar's, and their
//! selection-change messages arrive as `OnComboxBoxSelection` and `OnListBoxSelect`. The
//! column has no window of its own, which is why showing it is a loop over its controls
//! rather than one `ShowWindow`.

use crate::commands::{self, CommandStore};
use crate::console;
use crate::docs;
use crate::input;
use crate::state;
use std::ffi::c_void;

/// The row that divides a command's own parameters from the ones every cmdlet has. The original
/// ISE gave those a tab of their own; this column has room for one list, so it says where the
/// boundary is instead.
const COMMON_DIVIDER: &str = "-- common parameters --";

/// Which row a list box is scrolled to, as `LB_GETTOPINDEX` and `LB_SETTOPINDEX`. The numbers are
/// the ones in the Windows headers (398 and 407); the neighbouring 394 is `LB_GETTEXTLEN`, which
/// answers a completely different question and silently returns the length of a row's text.
const LB_GETTOPINDEX: u32 = 0x018E;
const LB_SETTOPINDEX: u32 = 0x0197;

/// The narrowest the column is ever drawn. Below this a long command name (`Get-ComputerRestorePoint`
/// at the column's font) is mostly ellipsis, and the whole point of the column is to recognise the
/// command by its name.
pub const MIN_WIDTH: i32 = 360;

/// The width the Commands column keeps, whatever the window is doing. The panes on its left give
/// it up and take the rest: a maximised window is wider, not more column, and the script and
/// console panes are the ones that wanted the room.
pub const COLUMN_WIDTH: i32 = 360;

/// How wide the column is allowed to be. It does not follow the window: the panes beside it are
/// what grow when the window does, so a maximised window is wider rather than more column. The
/// one thing the window's own width still decides is whether the column fits at all, so a window
/// too narrow to hold it is told the column is as narrow as it can be rather than being handed
/// one that cannot fit. The margin is the same `BORDER` the panes take on their left, so the two
/// calculations agree about what space is left.
pub fn width_for(client_width: i32) -> i32 {
    COLUMN_WIDTH.min((client_width - 2 * MARGIN).max(MIN_WIDTH))
}

const MARGIN: i32 = 2;

const PAD: i32 = 4;
const GAP: i32 = 4;
/// The height of a single-line control: the Module dropdown, the Find box, the value box, the
/// Use tick. It tracks the column's font, which is taller than it was, and a combo box whose
/// height is less than its font plus its own border draws its text clipped at the top.
const ROW_H: i32 = 26;
/// One line of the parameter note, which is the same font and therefore needs the same room.
const NOTE_H: i32 = 20;
const BUTTON_H: i32 = 26;

/// The first entry of the Module dropdown: show every command whatever it came from.
const ALL_MODULES: &str = "(All modules)";
/// The entry for the commands no module owns - the ones built into PowerShell, and the stubs
/// the session has a name for and nothing else.
const NO_MODULE: &str = "(no module)";

/// What the column is showing and what the user has chosen, plus the controls that show it.
///
/// The tick state and the typed values belong to a command, not to the panel, so they are
/// thrown away the moment another command is selected: a `-Force` ticked against one cmdlet
/// must not silently reappear against the next one.
#[derive(Default)]
pub struct Commands {
    pub store: CommandStore,
    /// Which Module dropdown entry is picked. 0 is `ALL_MODULES`, `n` is module `n - 1`.
    module: usize,
    /// The text in the Find box.
    find: String,
    /// The command rows the filters leave, as indexes into the store's entries. The list box
    /// shows these, so a row number and an entry number are not the same thing.
    rows: Vec<usize>,
    /// The selected row, as an index into `rows`.
    selected: Option<usize>,
    /// The rows the parameter list is showing, so a tick can be drawn in the row text without
    /// asking the store again.
    parameters: Vec<String>,
    /// Which parameter each parameter-list row stands for, or `None` for the row that only
    /// divides the command's own parameters from the ones every cmdlet has. The two are kept
    /// apart because the tick and the typed value belong to a parameter, while the list box
    /// counts rows.
    params_map: Vec<Option<usize>>,
    /// The selected row of the parameter list.
    parameter: Option<usize>,
    /// One per parameter, in list order: whether the command line should use it.
    chosen: Vec<bool>,
    /// One per parameter: the text in the value box, kept here so moving between parameters
    /// keeps what was typed.
    values: Vec<String>,
    /// The syntax area's text, so a repaint can be skipped when it has not changed. Rewriting
    /// an edit control the user may be reading would show as flicker.
    shown: String,

    module_box: nwg::ComboBox<String>,
    find_box: nwg::TextInput,
    name_list: nwg::ListBox<String>,
    syntax_box: nwg::TextBox,
    param_list: nwg::ListBox<String>,
    include_box: nwg::CheckBox,
    note_label: nwg::Label,
    value_box: nwg::TextInput,
    insert_button: nwg::Button,
    run_button: nwg::Button,
    copy_button: nwg::Button,
}

impl Commands {
    /// The module the list is filtered to, or `None` for all of them. `Some("")` means the
    /// commands no module owns, which is what `NO_MODULE` stands for.
    fn module_filter(&self) -> Option<&str> {
        match self.module {
            0 => None,
            index => self.store.modules().get(index - 1).map(|name| name.as_str()),
        }
    }

    /// The command the selected row stands for.
    fn selected_name(&self) -> Option<&str> {
        self.rows
            .get(self.selected?)
            .and_then(|&index| self.store.entries().get(index))
            .map(|entry| entry.name.as_str())
    }

    /// The command the selected row stands for, as an index into the store's entries. Stable
    /// across a rebuild of the list, which a row number is not.
    fn selected_command(&self) -> Option<usize> {
        self.selected.and_then(|row| self.rows.get(row)).copied()
    }

    /// Which parameter the selected row of the parameter list stands for. Nothing when the row
    /// is the divider, or when the list has no selection.
    fn selected_index(&self) -> Option<usize> {
        let row = self.parameter?;
        Some(*self.params_map.get(row)?.as_ref()?)
    }

    /// The parameter the list has selected, with its state: the tick and the typed value, or
    /// nothing when the selected row is the divider or the list is empty.
    fn selected_parameter(&self) -> Option<(&commands::ParameterInfo, bool, String)> {
        let index = self.selected_index()?;
        let detail = self.store.detail(self.selected_name()?)?;
        let parameter = detail.parameters.get(index)?;
        Some((
            parameter,
            self.chosen.get(index).copied().unwrap_or(false),
            self.values.get(index).cloned().unwrap_or_default(),
        ))
    }

    /// The parameters of the selected command, once they have been read.
    fn parameters_of(&self, name: &str) -> &[commands::ParameterInfo] {
        self.store
            .detail(name)
            .map(|detail| detail.parameters.as_slice())
            .unwrap_or(&[])
    }
}

/// Builds the column's controls as children of the main window. They are positioned by
/// [`layout`] and shown or hidden by it too, because the View menu and a window resize both
/// have to be able to change that.
pub fn create(window: &nwg::Window, font: Option<&nwg::Font>) -> Commands {
    let mut panel = Commands::default();

    // A dropdown the user chooses from rather than types in: the names come from the session,
    // and a module typed wrong would only look like a session with no commands in it.
    if let Err(e) = nwg::ComboBox::builder()
        .parent(window)
        .font(font)
        .collection(vec![ALL_MODULES.to_string()])
        .selected_index(Some(0))
        .build(&mut panel.module_box)
    {
        state::set_status(&format!("Could not create the Modules box ({})", e));
    }

    // The Find box filters as it is typed. It is a single-line edit: the original ISE's Find
    // in this column is a name prefix, and a plain substring is friendlier than nothing.
    if let Err(e) = nwg::TextInput::builder()
        .parent(window)
        .font(font)
        .build(&mut panel.find_box)
    {
        state::set_status(&format!("Could not create the Find box ({})", e));
    }

    if let Err(e) = nwg::ListBox::builder()
        .parent(window)
        .font(font)
        .build(&mut panel.name_list)
    {
        state::set_status(&format!("Could not create the command list ({})", e));
    }

    // The syntax area is read-only and scrolls: a cmdlet with six parameter sets is six long
    // lines, and the user has to be able to read all of them.
    if let Err(e) = nwg::TextBox::builder()
        .parent(window)
        .font(font)
        .flags(nwg::TextBoxFlags::VSCROLL | nwg::TextBoxFlags::AUTOVSCROLL)
        .readonly(true)
        .limit(64 * 1024)
        .build(&mut panel.syntax_box)
    {
        state::set_status(&format!("Could not create the syntax area ({})", e));
    }

    if let Err(e) = nwg::ListBox::builder()
        .parent(window)
        .font(font)
        .build(&mut panel.param_list)
    {
        state::set_status(&format!("Could not create the parameter list ({})", e));
    }

    if let Err(e) = nwg::CheckBox::builder()
        .parent(window)
        .font(font)
        .text("Use")
        .build(&mut panel.include_box)
    {
        state::set_status(&format!("Could not create the Use box ({})", e));
    }

    if let Err(e) = nwg::Label::builder()
        .parent(window)
        .font(font)
        .text("")
        .build(&mut panel.note_label)
    {
        state::set_status(&format!("Could not create the parameter note ({})", e));
    }

    if let Err(e) = nwg::TextInput::builder()
        .parent(window)
        .font(font)
        .build(&mut panel.value_box)
    {
        state::set_status(&format!("Could not create the value box ({})", e));
    }

    for (label, button) in [
        ("Insert", &mut panel.insert_button as &mut nwg::Button),
        ("Run", &mut panel.run_button),
        ("Copy", &mut panel.copy_button),
    ] {
        if let Err(e) = nwg::Button::builder()
            .parent(window)
            .font(font)
            .text(label)
            .build(button)
        {
            state::set_status(&format!("Could not create the {} button ({})", label, e));
        }
    }

    panel
}

/// Positions one control. Every nwg control has its own `set_position` and `set_size`, so
/// there is no common trait to take one by.
macro_rules! place {
    ($control:expr, $x:expr, $y:expr, $width:expr, $height:expr) => {{
        $control.set_position($x, $y);
        $control.set_size($width.max(1) as u32, $height.max(1) as u32);
    }};
}

/// Positions the column, and shows or hides it. Called from the frame's `layout`, which is
/// the only place the window's size is known — hence `width` rather than a constant: the column
/// is expected to grow with the window, and a width it invented itself is how it stayed 322 px
/// wide on a maximised window while the panes beside it doubled.
pub fn layout(panel: &mut Commands, x: i32, y: i32, width: i32, height: i32, visible: bool) {
    set_visible(panel, visible);
    if !visible {
        return;
    }

    let left = x + PAD;
    let inner = (width - 2 * PAD).max(80);
    let mut top = y + PAD;

    place!(panel.module_box, left, top, inner, ROW_H);
    top += ROW_H + GAP;
    place!(panel.find_box, left, top, inner, ROW_H);
    top += ROW_H + GAP;

    // The three blocks along the bottom are fixed; the two lists and the syntax area share
    // whatever is left, with the command list getting the largest share because it is the one
    // that is scanned rather than read.
    let mut bottom = (y + height - PAD).max(top + 60);
    let buttons_y = bottom - BUTTON_H;
    bottom = buttons_y - GAP;
    let value_y = bottom - ROW_H;
    bottom = value_y - GAP;
    let note_y = bottom - NOTE_H;
    bottom = note_y - GAP;

    let room = (bottom - top).max(120);
    let names_h = (room * 34 / 100).max(40);
    let syntax_h = (room * 30 / 100).max(40);
    let params_h = (bottom - top - names_h - syntax_h - 2 * GAP).max(40);

    place!(panel.name_list, left, top, inner, names_h);
    place!(
        panel.syntax_box,
        left,
        top + names_h + GAP,
        inner,
        syntax_h
    );
    place!(
        panel.param_list,
        left,
        top + names_h + syntax_h + 2 * GAP,
        inner,
        params_h
    );

    place!(panel.note_label, left, note_y, inner, NOTE_H);

    // The tick and the value belong to whichever parameter is selected, so they are one pair
    // of controls rather than a row per parameter: an Azure cmdlet reaches fifty-eight of
    // those, and fifty-eight edit boxes would be fifty-eight places for text to get lost.
    let use_w = inner / 4;
    place!(panel.include_box, left, value_y, use_w, ROW_H);
    place!(
        panel.value_box,
        left + use_w + GAP,
        value_y,
        inner - use_w - GAP,
        ROW_H
    );

    let button_w = (inner - 2 * GAP) / 3;
    place!(panel.insert_button, left, buttons_y, button_w, BUTTON_H);
    place!(
        panel.run_button,
        left + button_w + GAP,
        buttons_y,
        button_w,
        BUTTON_H
    );
    place!(
        panel.copy_button,
        left + 2 * (button_w + GAP),
        buttons_y,
        button_w,
        BUTTON_H
    );
}

/// Shows or hides every control in the column.
fn set_visible(panel: &mut Commands, visible: bool) {
    panel.module_box.set_visible(visible);
    panel.find_box.set_visible(visible);
    panel.name_list.set_visible(visible);
    panel.syntax_box.set_visible(visible);
    panel.param_list.set_visible(visible);
    panel.include_box.set_visible(visible);
    panel.note_label.set_visible(visible);
    panel.value_box.set_visible(visible);
    panel.insert_button.set_visible(visible);
    panel.run_button.set_visible(visible);
    panel.copy_button.set_visible(visible);
}

// ------------------------------------------------------------------ talking to the engine

/// Moves any command-metadata answer into the column, and asks for the command list when the
/// column is showing and the shell is idle.
///
/// This runs from the pump tick, and that is the only right place: the answers arrive on the
/// engine's reader thread, four thousand names must not be applied to a window from the thread
/// that produced them, and a request sent while a command is running is refused by the engine
/// rather than queued behind it.
pub fn pump() {
    // The wait out after a refusal is counted here, because this is the one thing that runs
    // exactly once per tick.
    state::write(|s| s.commands.store.tick());

    let ask = state::read(|s| {
        s.show_commands && !s.busy && s.engine.is_some() && s.commands.store.index_wanted()
    })
    .unwrap_or(false);
    if ask {
        state::write(|s| {
            let hard = s.commands.store.hard_refresh();
            s.commands.store.note_index_requested();
            if let Some(engine) = s.engine.as_mut() {
                if hard {
                    engine.reread_commands();
                } else {
                    engine.read_commands();
                }
            }
        });
    }

    let replies = state::write(|s| match s.engine.as_mut() {
        Some(engine) => engine.drain_metadata(4),
        None => Vec::new(),
    })
    .unwrap_or_default();
    if !replies.is_empty() {
        let mut lists = false;
        let mut count = 0usize;
        state::write(|s| {
            for reply in replies {
                // Whether the command list itself changed is what decides if the Name box has to
                // be rewritten: a parameter answer about one command should not rebuild four
                // thousand rows out from under the user's pointer.
                let listed_before = s.commands.store.is_listed();
                if state::debug_enabled() {
                    state::log_line(&format!(
                        "commands: {:?} answer, {} characters",
                        reply.kind,
                        reply.payload.chars().count()
                    ));
                }
                s.commands.store.apply(reply);
                if s.commands.store.is_listed() != listed_before {
                    lists = true;
                    count = s.commands.store.entries().len();
                }
            }
            if lists {
                reload_modules(s);
                reload_names(s);
            }
            reload_selection(s);
        });
        // The status bar is reading "Refreshing the command list..." since the menu item was
        // pressed, and nothing else in the program has reason to change it: a command that is
        // not running never reaches "Ready". Say what the read found instead.
        if lists {
            state::set_status(&format!("{} commands", count));
        }
    }

    ask_detail();
}

/// Asks again for the selected command's parameters when they have not arrived.
///
/// Picking the row asks once, and the engine refuses a read while the runspace is held - by a
/// command, by a startup script, or by the list read that is still on its way. Without this the
/// syntax area would go on saying `Reading this command...` for a read that was refused five
/// minutes ago, with nothing left for the user but to click another row and back again.
fn ask_detail() {
    state::write(|s| {
        if !s.show_commands || s.busy || s.engine.is_none() || s.commands.store.waiting() {
            return;
        }
        let Some(name) = s.commands.selected_name().map(str::to_string) else {
            return;
        };
        if !s.commands.store.detail_wanted(&name) {
            return;
        }
        // Recorded and sent in one go: a pending read that was never sent would leave the panel
        // waiting for an answer that was never asked for.
        s.commands.store.note_detail_requested(&name);
        if let Some(engine) = s.engine.as_mut() {
            engine.read_command(&name);
        }
    });
}

/// Shows or hides the column from the View menu. The first showing is what makes the engine
/// enumerate the session, because reading the list costs about a second and there is no reason
/// to pay it while the column is hidden.
pub fn toggle() {
    let showing = state::write(|s| {
        s.show_commands = !s.show_commands;
        s.show_commands
    })
    .unwrap_or(false);
    crate::layout();
    if showing {
        pump();
    }
}

/// Throws away what the panel knows and asks the engine to read the session again rather than
/// answer from its cache. Importing a module adds commands to the session, and the original
/// ISE had the same need under the same name.
pub fn refresh() {
    state::write(|s| s.commands.store.ask_refresh());
    state::set_status("Refreshing the command list...");
    // Sent now if the shell is idle; if it is busy, the next idle tick sends it, because
    // `ask_refresh` left the list unread and `pump` asks for what is missing.
    pump();
}

/// Refills the Module dropdown from the index. The selection is kept by module rather than by
/// position, so a refresh does not silently change which module is being filtered to - and
/// falls back to all of them when a module has gone away.
fn reload_modules(s: &mut state::AppState) {
    let previous = s.commands.module_filter().map(|name| name.to_string());

    let mut items = vec![ALL_MODULES.to_string()];
    items.extend(
        s.commands
            .store
            .modules()
            .iter()
            .filter(|name| !s.commands.store.broken().contains_key(*name))
            .map(|name| module_label(name).to_string()),
    );
    s.commands.module_box.set_collection(items);

    let keep = previous
        .as_deref()
        .and_then(|name| {
            s.commands
                .store
                .modules()
                .iter()
                .position(|module| module == name)
        })
        .map_or(0, |index| index + 1);
    s.commands.module = keep;
    s.commands.module_box.set_selection(Some(keep));
}

/// Puts the filtered names into the list box.
///
/// The selection follows the *command* it was on whenever that command is still on show, which
/// is what makes typing in Find survivable: the list is rebuilt on every keystroke and a list
/// box reset throws away its selection. When the command does change, everything chosen against
/// the previous one is dropped with it.
fn reload_names(s: &mut state::AppState) {
    let rows = s.commands.store.visible(s.commands.module_filter(), &s.commands.find);
    let names: Vec<String> = rows
        .iter()
        .filter_map(|&index| s.commands.store.entries().get(index))
        .map(|entry| entry.name.clone())
        .collect();

    let before = s.commands.selected_command();
    s.commands.rows = rows;
    s.commands.name_list.set_collection(names);
    s.commands.selected = before
        .and_then(|entry| s.commands.rows.iter().position(|&index| index == entry));
    s.commands.name_list.set_selection(s.commands.selected);

    if s.commands.selected_command() != before {
        reset_command(s);
    }
}

/// The user has moved to a different command. Everything chosen against the previous one is
/// dropped, and this command's parameters are asked for unless the engine has already
/// described them - which is also what imports its module, exactly as choosing a command in
/// the original ISE did.
fn reset_command(s: &mut state::AppState) {
    s.commands.chosen.clear();
    s.commands.values.clear();
    s.commands.parameter = None;
    s.commands.parameters.clear();

    let Some(name) = s.commands.selected_name().map(|name| name.to_string()) else {
        return;
    };
    if s.commands.store.detail_wanted(&name) {
        s.commands.store.note_detail_requested(&name);
        if let Some(engine) = s.engine.as_mut() {
            engine.read_command(&name);
        }
    }
}

/// Redraws everything below the command list: the syntax area, the parameter list, and the row
/// of controls that belongs to the selected parameter.
fn reload_selection(s: &mut state::AppState) {
    let text = syntax_text(&s.commands);
    if text != s.commands.shown {
        s.commands.shown = text.clone();
        // `set_text_unix2dos` because a multiline edit only breaks a line on CRLF, and this
        // file keeps its line breaks as LF like the rest of the program.
        s.commands.syntax_box.set_text_unix2dos(&text);
    }
    reload_parameters(s);
}

/// Refills the parameter list for the selected command. Rebuilt rather than patched: the
/// tick state lives in the row text, so a change anywhere in the command means every row has
/// to be written, and fifty-eight rows is nothing.
fn reload_parameters(s: &mut state::AppState) {
    fit_choices(s);
    let (rows, map): (Vec<String>, Vec<Option<usize>>) = match s
        .commands
        .selected_name()
        .map(|name| name.to_string())
    {
        Some(name) => {
            let parameters = s.commands.parameters_of(&name);
            let mut rows = Vec::with_capacity(parameters.len() + 1);
            let mut map = Vec::with_capacity(parameters.len() + 1);
            // The store already puts the ones every cmdlet has last; this says so out loud,
            // because the original ISE kept them on a tab of their own and a long list
            // otherwise reads as if `-Verbose` were something the command invented. The list
            // box cannot grey out a row, so a divider row is the next best thing, and the row
            // stands for no parameter of its own.
            let divider = parameters.iter().position(|parameter| parameter.is_common);
            for (index, parameter) in parameters.iter().enumerate() {
                if Some(index) == divider {
                    rows.push(COMMON_DIVIDER.to_string());
                    map.push(None);
                }
                let chosen = s.commands.chosen.get(index).copied().unwrap_or(false);
                rows.push(row_text(parameter, chosen));
                map.push(Some(index));
            }
            (rows, map)
        }
        None => (Vec::new(), Vec::new()),
    };

    // Replacing a list box's contents starts it at the top, so without this a tick on the
    // twentieth parameter of a long command threw the view back to the first row.
    let top = list_top_index(&s.commands.param_list);
    s.commands.parameters = rows.clone();
    s.commands.params_map = map;
    s.commands.param_list.set_collection(rows);
    s.commands.param_list.set_selection(s.commands.parameter);
    set_list_top_index(&s.commands.param_list, top);
    sync_parameter(s);
}

/// The row a list box is scrolled to, so a rebuild can put the view back.
fn list_top_index(list: &nwg::ListBox<String>) -> isize {
    match list.handle.hwnd() {
        Some(hwnd) => console::send_message(hwnd as *mut c_void, LB_GETTOPINDEX, 0, 0),
        None => 0,
    }
}

fn set_list_top_index(list: &nwg::ListBox<String>, top: isize) {
    if top <= 0 {
        return;
    }
    if let Some(hwnd) = list.handle.hwnd() {
        console::send_message(hwnd as *mut c_void, LB_SETTOPINDEX, top as usize, 0);
    }
}

/// Lines the tick, the value box and the note up with the selected parameter.
fn sync_parameter(s: &mut state::AppState) {
    // Read everything first: the parameter is borrowed from the state the controls are then
    // written through.
    let selected = s.commands.selected_parameter();
    let (note, chosen, value, is_switch, has) = match &selected {
        Some((parameter, chosen, value)) => (
            parameter_note(parameter),
            *chosen,
            value.clone(),
            parameter.is_switch,
            true,
        ),
        None => (String::new(), false, String::new(), false, false),
    };

    let panel = &mut s.commands;
    panel.include_box.set_enabled(has);
    panel.include_box.set_check_state(if chosen {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    });
    // A switch has no value to type, and a box that accepts one and then ignores it is worse
    // than a greyed-out one.
    panel.value_box.set_enabled(has && !is_switch);
    panel.value_box.set_text(&value);
    panel.note_label.set_text(&note);
}

/// What the syntax area says: what the command is, where it came from, what it is for, how it
/// is called, and - if the panel is waiting or has failed - that as well.
fn syntax_text(panel: &Commands) -> String {
    let Some(name) = panel.selected_name() else {
        if let Some(error) = panel.store.error() {
            return error.to_string();
        }
        if !panel.store.is_listed() {
            return "Reading the session's commands...".to_string();
        }
        // Nothing is selected. When the list itself is empty the filters are why, and saying so
        // beats telling the user to pick a command that is not there.
        return if panel.rows.is_empty() {
            no_match_text(panel)
        } else {
            "Pick a command to see how it is called.".to_string()
        };
    };

    let Some(detail) = panel.store.detail(name) else {
        // Three attempts and the engine said no every time - a command the session has stopped
        // knowing about, most likely, since the list was read. Saying "Reading..." would leave
        // the user waiting on an answer that is not coming, so say what happened, in the
        // engine's words where it has any, and what brings the list back up to date.
        if panel.store.detail_abandoned(name) {
            let mut out = name.to_string();
            if let Some(error) = panel.store.error() {
                out.push_str("\n\n");
                out.push_str(error);
            }
            out.push_str("\n\nView > Refresh Command List reads the session again.");
            return out;
        }
        return format!("{}\n\nReading this command...", name);
    };

    let mut out = detail.name.clone();
    if !detail.module.is_empty() {
        out.push_str(&format!("  ({})", detail.module));
    }
    out.push('\n');
    if !detail.synopsis.is_empty() {
        out.push('\n');
        out.push_str(&detail.synopsis);
        out.push('\n');
    }
    for set in syntax_sets(&detail.syntax) {
        out.push('\n');
        out.push_str(&set);
    }
    if detail.parameters.is_empty() {
        out.push_str("\n\nThis command takes no parameters.");
    } else {
        // The list has one row more than the command has parameters, because it says where the
        // ones every cmdlet has begin, so say how many of each there are rather than let the two
        // counts look like a mistake.
        let common = detail
            .parameters
            .iter()
            .filter(|parameter| parameter.is_common)
            .count();
        out.push_str(&format!(
            "\n\n{} parameters{}, tick the ones to use.",
            detail.parameters.len(),
            if common == 0 {
                String::new()
            } else {
                format!(", {} of them common", common)
            }
        ));
    }
    if let Some(error) = panel.store.error() {
        out.push_str("\n\n");
        out.push_str(error);
    }
    out
}

/// The line for a command list the filters have emptied. "Nothing matches" on its own leaves the
/// user guessing which of the two boxes to change, so this names the one that did it, and when
/// the words they typed do match commands elsewhere it says how many.
fn no_match_text(panel: &Commands) -> String {
    const TAIL: &str = "\n\nClear the Find box, or pick another module.";
    let find = panel.find.trim();
    let module = panel.module_filter();
    if find.is_empty() {
        // A module with nothing in it cannot happen straight from a refresh - a module is only
        // in the dropdown because a command named it - but can once a module leaves the session
        // while it is the one being filtered to.
        return match module {
            Some(name) => format!(
                "{} has no commands to list.\n\nPick another module, or choose {}.",
                module_label(name),
                ALL_MODULES
            ),
            None => "The session has no commands to list.".to_string(),
        };
    }
    match module {
        None => format!("No command matches {}.", find) + TAIL,
        // A module every one of whose commands the Find box has filtered away.
        Some(name) => {
            let elsewhere = panel.store.visible(None, find).len();
            let mut out = format!(
                "No command in {} matches {}. It has {} commands.",
                module_label(name),
                find,
                panel.store.visible(Some(name), "").len()
            );
            if elsewhere == 0 {
                return out + TAIL;
            }
            // The words they typed are in the session, just not here - which is worth saying,
            // and says which box to change.
            out.push_str(&format!(
                " {} {} in other modules.\n\nChoose {} to see them.",
                elsewhere,
                if elsewhere == 1 {
                    "command matches"
                } else {
                    "commands match"
                },
                ALL_MODULES
            ));
            out
        }
    }
}

/// How a module is written to the user. The commands no module owns have no name of their own,
/// so they are shown by the same label the dropdown gives them.
fn module_label(name: &str) -> &str {
    if name.is_empty() {
        NO_MODULE
    } else {
        name
    }
}

/// The parameter sets out of the engine's syntax field. `Get-Command`'s definition holds one
/// per line with blank lines above, below and between them; here the blank lines are the
/// separator rather than the padding, so the block starts with the command instead of with
/// nothing.
fn syntax_sets(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

/// One row of the parameter list. The tick is part of the text: a list box cannot own-draw a
/// check box column without a parent window of its own to take the drawing messages, and the
/// state is as legible written in the row as drawn beside it. A star marks a parameter the
/// command cannot be run without, which is why the list puts those first.
fn row_text(parameter: &commands::ParameterInfo, chosen: bool) -> String {
    format!(
        "{} {}{}  <{}>",
        if chosen { "[x]" } else { "[ ]" },
        parameter.name,
        if parameter.mandatory { " *" } else { "" },
        parameter.type_name
    )
}

/// The line under the parameter list: the type, and whether the parameter is required,
/// positional, a switch, or one of the ones every cmdlet has. The syntax area says all of that
/// in the engine's own words; this says it for the one row the caret is on.
fn parameter_note(parameter: &commands::ParameterInfo) -> String {
    let mut bits: Vec<String> = vec![parameter.type_name.clone()];
    if parameter.mandatory {
        bits.push("required".to_string());
    }
    if let Some(position) = parameter.position {
        bits.push(format!("position {}", position));
    }
    // The type name the engine gives a switch is already `switch`, so saying it again only makes
    // the note read `-Debug  switch, switch`. It is worth the word when the type says otherwise,
    // because it is the fact that no value is wanted.
    if parameter.is_switch && !parameter.type_name.eq_ignore_ascii_case("switch") {
        bits.push("switch".to_string());
    }
    if parameter.is_common {
        bits.push("common".to_string());
    }
    if !parameter.description.is_empty() {
        bits.push(parameter.description.clone());
    }
    format!("-{}  {}", parameter.name, bits.join(", "))
}

// ------------------------------------------------------------------ the user

/// Handles a notification from one of the column's controls, and gives back true when the
/// event belonged to the panel.
///
/// The column's controls are children of the main window, so the frame is the window their
/// messages are sent to and it passes on everything it does not use itself. Which control sent
/// the message is the only thing that tells the two lists and the two edit boxes apart.
pub fn on_event(evt: nwg::Event, handle: &nwg::ControlHandle) -> bool {
    use nwg::Event as Evt;

    // Which control spoke is the whole question for a column built from stock controls, so it
    // is worth having in the log when a click appears to do nothing. Typing is left out: the
    // frame sends every edit control's change here, including the script pane's.
    if state::debug_enabled() && !matches!(evt, Evt::OnTextInput) {
        state::log_line(&format!("commands: event {:?}", evt));
    }

    match evt {
        Evt::OnComboxBoxSelection => {
            let index = state::read(|s| s.commands.module_box.selection())
                .flatten()
                .unwrap_or(0);
            if state::debug_enabled() {
                // The dropdown is the one control whose message could have come from something
                // else, and whether the box reports a selection at all decides what the list
                // shows, so both are worth having in the log.
                state::log_line(&format!(
                    "commands: module event from {:?}, box is {:?}, index {}",
                    handle,
                    state::read(|s| s.commands.module_box.handle),
                    index
                ));
            }
            if !state::is(handle, |s| s.commands.module_box.handle) {
                return false;
            }
            state::write(|s| {
                s.commands.module = index;
                reload_names(s);
                reload_selection(s);
            });
            true
        }

        Evt::OnListBoxSelect => {
            if state::is(handle, |s| s.commands.name_list.handle) {
                state::write(|s| {
                    select_name(s);
                    reload_selection(s);
                });
                true
            } else if state::is(handle, |s| s.commands.param_list.handle) {
                state::write(|s| {
                    s.commands.parameter = s.commands.param_list.selection();
                    sync_parameter(s);
                });
                true
            } else {
                false
            }
        }

        // Both double-clicks are the two-click version of what the controls along the bottom
        // do, so the pointer does not have to travel the length of the column.
        Evt::OnListBoxDoubleClick => {
            if state::is(handle, |s| s.commands.name_list.handle) {
                insert();
                true
            } else if state::is(handle, |s| s.commands.param_list.handle) {
                toggle_use();
                true
            } else {
                false
            }
        }

        // A check box is a button as far as Windows is concerned, so its tick arrives here too.
        Evt::OnButtonClick => {
            if state::is(handle, |s| s.commands.include_box.handle) {
                toggle_use();
                true
            } else if state::is(handle, |s| s.commands.insert_button.handle) {
                insert();
                true
            } else if state::is(handle, |s| s.commands.run_button.handle) {
                run();
                true
            } else if state::is(handle, |s| s.commands.copy_button.handle) {
                copy();
                true
            } else {
                false
            }
        }

        Evt::OnTextInput => {
            if state::is(handle, |s| s.commands.find_box.handle) {
                let text = state::read(|s| s.commands.find_box.text()).unwrap_or_default();
                state::write(|s| {
                    s.commands.find = text;
                    reload_names(s);
                    reload_selection(s);
                });
                true
            } else if state::is(handle, |s| s.commands.value_box.handle) {
                let text = state::read(|s| s.commands.value_box.text()).unwrap_or_default();
                state::write(|s| {
                    fit_choices(s);
                    if let Some(index) = s.commands.selected_index() {
                        if let Some(value) = s.commands.values.get_mut(index) {
                            *value = text;
                        }
                    }
                });
                true
            } else {
                false
            }
        }

        _ => false,
    }
}

/// The row the user picked in the command list. A different command resets what was chosen
/// against the previous one.
fn select_name(s: &mut state::AppState) {
    let row = s.commands.name_list.selection();
    if row != s.commands.selected {
        s.commands.selected = row;
        reset_command(s);
    }
}

/// Ticks or unticks the selected parameter. The divider row stands for no parameter, so it
/// ticks nothing.
fn toggle_use() {
    state::write(|s| {
        fit_choices(s);
        let Some(index) = s.commands.selected_index() else {
            return;
        };
        let text = s.commands.value_box.text();
        if let Some(value) = s.commands.values.get_mut(index) {
            *value = text;
        }
        if let Some(chosen) = s.commands.chosen.get_mut(index) {
            *chosen = !*chosen;
        }
        reload_parameters(s);
    });
}

/// Keeps the tick and value lists exactly as long as the selected command's parameters. Both
/// are thrown away when the command changes, so a length mismatch only ever means this
/// command's parameters have just been read.
fn fit_choices(s: &mut state::AppState) {
    let count = match s.commands.selected_name().map(|name| name.to_string()) {
        Some(name) => s.commands.parameters_of(&name).len(),
        None => 0,
    };
    s.commands.chosen.resize(count, false);
    s.commands.values.resize(count, String::new());
}

/// The command line the column's choices add up to.
///
/// Parameters are always written by name and never positionally, as in the original ISE: a
/// command read back off the script pane has to say which value belongs to which parameter,
/// and it is edited by hand either way. They come out in list order, which puts the required
/// ones first.
fn composed(panel: &Commands) -> Option<String> {
    let name = panel.selected_name()?;
    let detail = panel.store.detail(name)?;
    let mut out = name.to_string();

    for (index, parameter) in detail.parameters.iter().enumerate() {
        if !panel.chosen.get(index).copied().unwrap_or(false) {
            continue;
        }
        out.push(' ');
        out.push('-');
        out.push_str(&parameter.name);
        if parameter.is_switch {
            continue;
        }
        let value = panel
            .values
            .get(index)
            .map(|value| argument(value))
            .unwrap_or_default();
        if !value.is_empty() {
            out.push(' ');
            out.push_str(&value);
        }
    }

    Some(out)
}

/// A value made safe to put on a command line: trimmed, and quoted when it is a bare phrase
/// with spaces in it. Anything that already looks like an expression - a variable, a quoted
/// string, a subexpression - is left exactly as typed, because quoting that would change what
/// it means rather than what it looks like.
fn argument(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if matches!(
        trimmed.chars().next(),
        Some('\'') | Some('"') | Some('$') | Some('(')
    ) || !trimmed.chars().any(|ch| ch.is_whitespace() || ch == ';')
    {
        return trimmed.to_string();
    }
    format!("'{}'", trimmed.replace('\'', "''"))
}

/// What the three buttons say when they have no command line to offer. A command that is
/// selected but has no description - the one the engine refused three times - is a different
/// problem from an empty list, and telling the user to pick a command they can see picked reads
/// as the panel disagreeing with their own eyes.
fn nothing_to_compose() {
    let picked = state::read(|s| s.commands.selected_name().is_some()).unwrap_or(false);
    if picked {
        state::beep();
        state::set_status("This command could not be read - see the syntax area");
    } else {
        state::set_status("Pick a command first");
    }
}

/// The line the column's choices add up to, or nothing when no command is chosen.
///
/// Logged either way: a button that quietly did nothing is the hardest thing to find in a
/// column made of stock controls, and "Insert did nothing" looks exactly like "Insert wrote an
/// empty line".
fn command_line(action: &str) -> Option<String> {
    let line = state::read(|s| composed(&s.commands)).flatten();
    if state::debug_enabled() {
        match &line {
            Some(line) => state::log_line(&format!("commands: {} `{}`", action, line)),
            None => state::log_line(&format!(
                "commands: {} had no command chosen",
                action
            )),
        }
    }
    line
}

/// Puts the command line where the caret is.
fn insert() {
    let Some(line) = command_line("Insert") else {
        nothing_to_compose();
        return;
    };

    // Which pane the caret was in is the whole decision, and it is invisible from outside:
    // neither pane having the focus is a normal state, not an error.
    let to_console = state::read(focus_is_console).unwrap_or(false);
    if state::debug_enabled() {
        state::log_line(&format!(
            "commands: insert goes to the {}",
            if to_console { "console" } else { "script" }
        ));
    }

    if to_console {
        // While a command is running the console's input line is closed, and the next prompt
        // throws away whatever was typed into it. Keystrokes are refused for the same reason,
        // so Insert refuses too rather than write a command line that would vanish.
        let open = state::read(|s| s.input_open).unwrap_or(false);
        if !open {
            if state::debug_enabled() {
                state::log_line("commands: insert refused, the console has no prompt");
            }
            // The Run button beeps for the same reason, and the status bar only holds this
            // message until the running command's next output replaces it.
            state::beep();
            state::set_status("The console is running a command - Insert waits for its prompt");
            return;
        }
        let caret = state::read(|s| s.draft_caret).unwrap_or(0);
        input::replace_range(caret, caret, &line);
        state::focus_console();
        state::set_status("Command inserted in the console");
        return;
    }
    insert_in_script(&line);
}

/// Splices text into the script pane at its selection.
///
/// The document is rewritten and pushed through the control rather than patched in place:
/// `EM_REPLACESEL` does nothing to these rich edit controls here, while `WM_SETTEXT` - which
/// is what `load_editor_from_doc` uses - always works. The caret survives because the program
/// already drives the selection to paint syntax colours, so both the old position and the new
/// one are known exactly.
fn insert_in_script(line: &str) {
    let (text, start, end) = state::read(|s| {
        let selection = s.editor.selection();
        (s.editor.text(), selection.start as usize, selection.end as usize)
    })
    .unwrap_or_default();

    // The control counts in UTF-16 units and the document is UTF-8, so the splice is done in
    // the control's units and converted back.
    let units: Vec<u16> = text.encode_utf16().collect();
    let start = start.min(units.len());
    let end = end.max(start).min(units.len());
    let mut spliced = units[..start].to_vec();
    spliced.extend(line.encode_utf16());
    spliced.extend_from_slice(&units[end..]);
    let text = String::from_utf16_lossy(&spliced);
    let caret = (start + line.encode_utf16().count()) as u32;

    state::write(|s| {
        if let Some(doc) = s.docs.get_mut(s.current) {
            doc.text = text;
        }
    });
    docs::load_editor_from_doc();
    state::write(|s| s.editor.set_selection(caret..caret));
    if state::debug_enabled() {
        // What the control holds afterwards is the only proof the write reached it: the pane is
        // a rich edit, and a rich edit that accepted a line and shows nothing looks the same as
        // one that refused it.
        state::log_line(&format!(
            "commands: script pane now holds {} characters, caret {}",
            docs::editor_text().chars().count(),
            caret
        ));
    }
    state::set_status("Command inserted in the script");
}

/// Runs the command line now, into the console, whether or not it was ever written into the
/// script - which is what the original ISE's Run button did, and why it is next to Insert
/// rather than in the Debug menu.
fn run() {
    let Some(line) = command_line("Run") else {
        nothing_to_compose();
        return;
    };
    docs::run_text(&line);
}

/// Puts the command line on the clipboard, for a shell somewhere else.
fn copy() {
    let Some(line) = command_line("Copy") else {
        nothing_to_compose();
        return;
    };
    input::write_clipboard(&line);
    state::set_status("Command copied");
}

extern "system" {
    /// The window on this thread holding the keyboard focus.
    fn GetFocus() -> *mut c_void;
}

/// True when the console pane has the caret, which is the one case where a command belongs on
/// the input line rather than in the script. Neither pane having it - the state the moment a
/// click lands on this column - goes to the script pane while it is showing, because that is
/// where a command is written up. And a pane that is hidden cannot take a command: with the
/// console hidden the caret still belongs to it, which is not the same as it being able to
/// show what was inserted.
fn focus_is_console(s: &state::AppState) -> bool {
    if !s.show_console {
        return false;
    }
    let focus = unsafe { GetFocus() };
    if focus.is_null() {
        return !s.show_script;
    }
    s.console.handle.hwnd().map(|h| h as *mut c_void) == Some(focus)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parameter(
        name: &str,
        type_name: &str,
        mandatory: bool,
        is_switch: bool,
    ) -> commands::ParameterInfo {
        commands::ParameterInfo {
            name: name.to_string(),
            type_name: type_name.to_string(),
            mandatory,
            position: None,
            is_switch,
            is_common: false,
            description: String::new(),
        }
    }

    /// What is typed in the value box goes out onto a command line, so these rules are the rules
    /// about what the shell will run.
    #[test]
    fn a_typed_value_is_quoted_only_when_it_needs_to_be() {
        assert_eq!(argument("C:\\temp"), "C:\\temp");
        assert_eq!(
            argument("  C:\\temp  "),
            "C:\\temp",
            "spaces around it are not part of the value"
        );
        assert_eq!(argument("two words"), "'two words'");
        assert_eq!(
            argument("it's here"),
            "'it''s here'",
            "a quote is doubled the way PowerShell does it"
        );
        // A semicolon would end the argument and start another statement.
        assert_eq!(argument("a;b"), "'a;b'");
        // Anything that is already an expression is left alone: quoting it would change what it
        // means, not how it looks.
        assert_eq!(argument("$path"), "$path");
        assert_eq!(argument("(Get-Location).Path"), "(Get-Location).Path");
        assert_eq!(argument("'already quoted'"), "'already quoted'");
        assert_eq!(argument("\"quoted\""), "\"quoted\"");
        assert_eq!(argument("   "), "", "nothing typed is nothing on the line");
    }

    #[test]
    fn parameter_sets_are_the_lines_the_blank_lines_separate() {
        let raw = "\nGet-ChildItem [[-Path] <string[]>]\n\nGet-ChildItem -LiteralPath <string[]>\n\n";
        assert_eq!(
            syntax_sets(raw),
            [
                "Get-ChildItem [[-Path] <string[]>]",
                "Get-ChildItem -LiteralPath <string[]>"
            ]
        );
        assert!(syntax_sets("").is_empty());
        assert!(syntax_sets("\n \t \n").is_empty());
    }

    #[test]
    fn a_parameter_row_shows_the_tick_the_star_and_the_type() {
        assert_eq!(
            row_text(&parameter("Path", "string[]", true, false), false),
            "[ ] Path *  <string[]>"
        );
        assert_eq!(
            row_text(&parameter("Recurse", "switch", false, true), true),
            "[x] Recurse  <switch>"
        );
    }
}
