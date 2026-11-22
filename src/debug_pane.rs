//! The Debugger column: where execution is held, the frames it is held between, what those
//! frames hold, and an Immediate box to ask about an expression.
//!
//! It is the column the original ISE's Debugger add-on was: three lists stacked in the strip
//! down the right-hand side, filled from the engine's `D` records. Everything shown here is a
//! copy of what the engine said while it was holding the shell, and all of it is thrown away
//! when the stop ends — see [`crate::debug::Model::clear_stop`].
//!
//! The three lists cannot be asked for from here at a sensible moment by the person using
//! them, because the window is not allowed to block: the answers arrive over several timer
//! ticks, interleaved with console output. So a stop asks for the call stack, the call stack
//! answers with the frame to read, and reading that frame fills the Variables box. Clicking a
//! frame is the one request made by hand.

use crate::debug::{self, DebugEvent, DebugKind, DebugReply, VariableRow};
use crate::state;

const PAD: i32 = 6;
const GAP: i32 = 5;
const ROW_H: i32 = 22;
const BUTTON_H: i32 = 23;
/// How much of the column the transcript of what was asked and answered keeps. It is a fixed
/// height rather than a share because its rows are read one after another, and a box that
/// shrinks to three lines on a short window is a box nobody reads.
const IMMEDIATE_H: i32 = 84;

/// The subclass id the Immediate box's Enter key is watched under. The two panes have their
/// own; this is the third control in the window that needs key press messages rather than the
/// character messages a text box normally reports.
const EXPR_SUBCLASS_ID: usize = 0x0002_0004;

/// The column's controls, and the rows they were last given.
///
/// The row copies are what keep a list box from being rebuilt every fortieth of a second: a
/// `set_collection` resets the selection and the scroll position, which on a twenty variable
/// frame means the pane jumps back to the top while the user is reading it.
#[derive(Default)]
pub struct Debugger {
    stack_rows: Vec<String>,
    var_rows: Vec<String>,
    /// The text each label was last given. A label whose text is set to what it already says
    /// still repaints, and these three are written on every tick that changes anything.
    status_text: String,
    stack_heading: String,
    vars_heading: String,
    /// What the Immediate box holds, so a tick that added nothing leaves it alone.
    immediate_text: String,
    status_label: nwg::Label,
    stack_label: nwg::Label,
    stack_list: nwg::ListBox<String>,
    var_label: nwg::Label,
    /// Names the transcript below it. It is the only box in the column with no heading of its
    /// own, and an empty unlabeled box in a debugger column reads as a pane that has broken.
    immediate_label: nwg::Label,
    var_list: nwg::ListBox<String>,
    immediate: nwg::TextBox,
    expr_box: nwg::TextInput,
    eval_button: nwg::Button,
}

/// Builds the column as a child of the main window, hidden: it is shown when there is
/// something to debug, and every control is created once here rather than on demand.
pub fn create(window: &nwg::Window, font: Option<&nwg::Font>) -> Debugger {
    let mut panel = Debugger::default();

    for (label, text) in [
        (&mut panel.status_label as &mut nwg::Label, "Not debugging"),
        (&mut panel.stack_label, "Call stack"),
        (&mut panel.var_label, "Variables"),
        (&mut panel.immediate_label, "Immediate"),
    ] {
        if let Err(e) = nwg::Label::builder()
            .parent(window)
            .font(font)
            .text(text)
            .build(label)
        {
            state::set_status(&format!("Could not create the debugger heading ({})", e));
        }
    }

    // Both lists scroll and neither is typed in. The call stack is single choice: it picks a
    // frame to read, and reading two at once is not a thing a debugger does.
    for (list, name) in [
        (&mut panel.stack_list as &mut nwg::ListBox<String>, "call stack"),
        (&mut panel.var_list, "variables"),
    ] {
        if let Err(e) = nwg::ListBox::builder()
            .parent(window)
            .font(font)
            .build(list)
        {
            state::set_status(&format!("Could not create the {} list ({})", name, e));
        }
    }

    // The Immediate box keeps what was asked and what came back, so an expression typed two
    // steps ago can still be read after the frame it was typed in has moved on. Read-only for
    // that reason: it is a transcript, and text edited in a transcript is a lie about what the
    // shell answered.
    //
    // It scrolls sideways rather than wrapping. Every exchange is two rows - the question, then
    // the answer under it - and a box that wraps a long answer onto two more rows turns six
    // questions into a paragraph nobody can take apart.
    if let Err(e) = nwg::TextBox::builder()
        .parent(window)
        .font(font)
        .flags(
            nwg::TextBoxFlags::VSCROLL
                | nwg::TextBoxFlags::AUTOVSCROLL
                | nwg::TextBoxFlags::HSCROLL
                | nwg::TextBoxFlags::AUTOHSCROLL,
        )
        .readonly(true)
        .limit(128 * 1024)
        .build(&mut panel.immediate)
    {
        state::set_status(&format!("Could not create the Immediate box ({})", e));
    }

    if let Err(e) = nwg::TextInput::builder()
        .parent(window)
        .font(font)
        .placeholder_text(Some("Evaluate in the stopped frame"))
        .build(&mut panel.expr_box)
    {
        state::set_status(&format!("Could not create the expression box ({})", e));
    }

    if let Err(e) = nwg::Button::builder()
        .parent(window)
        .font(font)
        .text("Evaluate")
        .build(&mut panel.eval_button)
    {
        state::set_status(&format!("Could not create the Evaluate button ({})", e));
    }

    // Enter asks the question. A text box reports characters rather than keys, so this control
    // gets a hook of its own for the one key that means something different from typing it.
    if let Ok(handler) = nwg::bind_raw_event_handler(
        &panel.expr_box.handle,
        EXPR_SUBCLASS_ID,
        move |_hwnd, msg, w, _l| {
            if msg == crate::console::WM_KEYDOWN && w as u32 == 0x0D {
                evaluate_from_box();
                return Some(0);
            }
            None
        },
    ) {
        // Held for the lifetime of the process, like the panes' hooks.
        std::mem::forget(handler);
    }

    set_visible(&mut panel, false);
    panel
}

/// Positions the column, and shows or hides it. Called from the frame's `layout`.
macro_rules! place {
    ($control:expr, $x:expr, $y:expr, $width:expr, $height:expr) => {{
        $control.set_position($x, $y);
        $control.set_size($width.max(1) as u32, $height.max(1) as u32);
    }};
}

pub fn layout(panel: &mut Debugger, x: i32, y: i32, width: i32, height: i32, visible: bool) {
    set_visible(panel, visible);
    if !visible {
        return;
    }

    let left = x + PAD;
    let inner = (width - 2 * PAD).max(80);
    let mut top = y + PAD;

    place!(panel.status_label, left, top, inner, ROW_H);
    top += ROW_H + GAP;

    // What the bottom of the column holds is fixed: the expression row with its button, and the
    // Immediate box under a heading of its own. The two lists share the rest, the call stack
    // taking the smaller share because it is scanned for one frame rather than read.
    //
    // None of it may overlap. These are siblings rather than children of one another, so a list
    // painted over the box below it does not push that box out of the way - it hides it, which is
    // how the Immediate transcript disappeared behind the variable list when the Commands column
    // was showing above it. So the transcript gives its height back before the lists are put
    // where the expression row is: with Commands stacked over this column there is often only
    // just room for the two lists it exists to show.
    let mut bottom = (y + height - PAD).max(top);
    let expr_y = bottom - ROW_H;
    bottom -= ROW_H + GAP;

    // Two headings, three gaps, and two lists short enough to still show a row each.
    const LISTS_MIN: i32 = 2 * ROW_H + 3 * GAP + 2 * 36;

    let room = (bottom - top).max(0);
    let immediate_h = if room >= IMMEDIATE_H + ROW_H + GAP + LISTS_MIN {
        IMMEDIATE_H
    } else {
        (room - ROW_H - GAP - LISTS_MIN).max(0)
    };
    let immediate_y = bottom - immediate_h;
    bottom = immediate_y - GAP;
    let immediate_label_y = bottom - ROW_H;
    bottom = immediate_label_y - GAP;

    let lists = (bottom - top - 2 * ROW_H - 3 * GAP).max(0);
    let stack_h = lists * 38 / 100;
    let vars_h = lists - stack_h;

    place!(panel.stack_label, left, top, inner, ROW_H);
    place!(panel.stack_list, left, top + ROW_H + GAP, inner, stack_h);

    let vars_top = top + ROW_H + GAP + stack_h + GAP;
    place!(panel.var_label, left, vars_top, inner, ROW_H);
    place!(
        panel.var_list,
        left,
        vars_top + ROW_H + GAP,
        inner,
        vars_h
    );

    place!(panel.immediate_label, left, immediate_label_y, inner, ROW_H);
    place!(panel.immediate, left, immediate_y, inner, immediate_h);

    let button_w = (inner / 4).max(64);
    place!(
        panel.expr_box,
        left,
        expr_y,
        inner - button_w - GAP,
        ROW_H
    );
    place!(
        panel.eval_button,
        left + inner - button_w,
        expr_y,
        button_w,
        BUTTON_H
    );
}

/// Shows or hides every control in the column.
fn set_visible(panel: &mut Debugger, visible: bool) {
    panel.status_label.set_visible(visible);
    panel.stack_label.set_visible(visible);
    panel.stack_list.set_visible(visible);
    panel.var_label.set_visible(visible);
    panel.var_list.set_visible(visible);
    panel.immediate_label.set_visible(visible);
    panel.immediate.set_visible(visible);
    panel.expr_box.set_visible(visible);
    panel.eval_button.set_visible(visible);
}

/// Shows or hides the column from the View menu.
pub fn toggle() {
    let showing = state::write(|s| {
        s.show_debugger = !s.show_debugger;
        s.show_debugger
    });
    if let Some(showing) = showing {
        state::set_status(if showing {
            "Debugger column shown"
        } else {
            "Debugger column hidden"
        });
        let _ = showing;
    }
    crate::layout();
}

// ------------------------------------------------------------------ following the stop

/// Moves stop notifications and debugger answers into the column. Called from the pump tick.
///
/// Both queues are taken before anything is applied, and applied in the order the engine sent
/// them: a stop that began and ended between two ticks still has to have its answers applied
/// before the resume throws them away, or the pane ends up holding the variables of a frame
/// that is gone while the shell runs on beneath it.
pub fn pump() {
    // The margin and the column are both drawn from the shell's own list, so the first thing to
    // do once there is a shell is ask for it. Asked here rather than where the engine is
    // started, because that is also where a failed start is noticed - and a window with no
    // shell has no list to ask for.
    if state::write(|s| {
        if s.debug.listed || s.engine.is_none() {
            false
        } else {
            s.debug.listed = true;
            true
        }
    }) == Some(true)
    {
        state::list_breakpoints();
    }

    let events = state::write(|s| match s.engine.as_mut() {
        Some(engine) => engine.drain_debug_events(16),
        None => Vec::new(),
    })
    .unwrap_or_default();

    let replies = state::write(|s| match s.engine.as_mut() {
        Some(engine) => engine.drain_debug(8),
        None => Vec::new(),
    })
    .unwrap_or_default();

    if events.is_empty() && replies.is_empty() {
        return;
    }

    let mut reshown = false;

    // What the status bar has to say once this batch is applied. It is read out of the batch
    // before the batch is moved into the state, because the status bar is painted through the
    // same state and asking for it inside the borrow would be asking for it twice.
    //
    // It matters on a stop: the bar reads "Running... (Ctrl+C or the Stop button to interrupt)"
    // from the moment a script starts, and that advice is wrong the instant the debugger takes
    // hold of the script. The key to press then is F5, and the one it names abandons the session
    // and everything the stop was showing.
    let mut status: Option<Option<(i32, String)>> = None;
    for event in &events {
        match event {
            DebugEvent::Stopped(info) => {
                status = Some(Some((info.line, info.script.clone())));
            }
            DebugEvent::Resumed => status = Some(None),
        }
    }

    state::write(|s| {
        for event in events {
            reshown |= apply_event(s, event);
        }
        for reply in replies {
            apply_reply(s, reply);
        }
        repaint(s);
    });

    match status {
        Some(Some((line, script))) => {
            // The script pane follows the stop, without taking the focus: the line execution came
            // to rest on is often a screen below what the pane is showing, and an arrow the user
            // cannot see is a stop they cannot see, because the margin only marks the rows that
            // are on screen. Only when the tab really is the file execution stopped in — three
            // scripts can be open and the run belongs to one of them.
            let here = state::current_script_path();
            if here
                .as_deref()
                .is_some_and(|path| path.eq_ignore_ascii_case(&script))
            {
                state::reveal_line(line);
            }

            let script = short_name(&script);
            state::set_status(&format!(
                "Debugger stopped at line {} of {} - F5 continue, F10 step over, Ctrl+C abandons",
                line, script
            ));
        }
        // Let go: the script is running again, and the sentence is the one the console uses for a
        // running script. The test matters - letting go of the last breakpoint and being asked to
        // interrupt a script that finished in the same tick are different situations, and the bar
        // has to say which. This pump runs after the engine's, so an unconditional word here would
        // arrive after the console has already reported "Ready" and would overwrite the truth.
        // Note the third arm too: a batch of nothing but answers leaves the bar alone, or a
        // breakpoint's "Set at line 6" would be spoken over by a reply.
        Some(None) => {
            if state::read(|s| s.busy).unwrap_or(false) {
                state::set_status("Running... (Ctrl+C or the Stop button to interrupt)");
            }
        }
        None => {}
    }

    // A stop shows the column when it is hidden, and hiding it has to be undone by the same
    // layout pass a resize would have done.
    if reshown {
        crate::layout();
    }
}

/// Writes what the column is holding into the debug log, through the `{DUMP}` inject token.
///
/// A test cannot read these strings off the window: the two panes are RichEdit, whose text is a lie
/// to another process — the project's own note on verifying by hand — and the strip's labels and
/// list boxes turn out to answer `GetWindowText` no better. So the window says it instead.
///
/// What is written is not a second opinion about the controls but the very text `repaint` handed
/// them, from the same fields: a check that passes here is a check on what is on the screen, and one
/// that fails means the screen says it too.
pub fn dump_to_log() {
    let rows = state::read(|s| {
        let mut rows = vec![
            format!("dump: visible {}", s.show_debugger),
            format!("dump: status {}", s.debugger.status_text),
            format!("dump: heading {}", s.debugger.stack_heading),
        ];
        rows.extend(
            s.debugger
                .stack_rows
                .iter()
                .map(|row| format!("dump: stack {}", row)),
        );
        rows.extend(
            s.debugger
                .var_rows
                .iter()
                .map(|row| format!("dump: variable {}", row)),
        );
        // Split on the pair the box is joined with, so a transcript whose rows have run together is
        // one row here and several there. That is what the line-endings fix was for, and this is
        // how a test can tell.
        rows.extend(
            s.debugger
                .immediate_text
                .split("\r\n")
                .map(|row| format!("dump: answer {}", row)),
        );
        rows
    })
    .unwrap_or_default();

    state::log_line("dump: begin");
    for row in rows {
        state::log_line(&row);
    }
}

/// Applies one stop or resume. Gives back true when the column had to be shown, which is the
/// one change the caller has to lay out for.
fn apply_event(state: &mut state::AppState, event: DebugEvent) -> bool {
    match event {
        DebugEvent::Stopped(info) => {
            if state::debug_enabled() {
                state::log_line(&format!(
                    "debug: stopped ({}) at {}:{} col {}",
                    info.reason, info.script, info.line, info.column
                ));
            }
            state.debug.stop = Some(info);
            state.debug.frame = 0;
            state.debug.frames.clear();
            state.debug.variables.clear();
            state.debug.note.clear();

            // The frames first, because the frame number is what a variables request asks for,
            // and reading frame 0 while the user is about to click frame 3 costs one answer
            // that is superseded a moment later rather than a pane that shows nothing.
            if let Some(engine) = state.engine.as_mut() {
                engine.debug_request("k", "");
                engine.debug_request("v", "0");
            }

            let was_hidden = !state.show_debugger;
            state.show_debugger = true;
            was_hidden
        }

        DebugEvent::Resumed => {
            state.debug.clear_stop();
            crate::gutter::invalidate();
            false
        }
    }
}

/// Applies one answer about the stop.
fn apply_reply(state: &mut state::AppState, reply: DebugReply) {
    match reply.kind {
        DebugKind::Breakpoints => {
            let (points, errors) = debug::parse_breakpoints(&reply.payload);
            state.debug.breakpoints = points;
            state.debug.note = errors.join("\n");
            // The gutter draws from this list, and it caches what it last painted.
            crate::gutter::invalidate();
        }

        DebugKind::CallStack => {
            let frames = debug::parse_call_stack(&reply.payload);
            if frames.is_empty() {
                // An empty stack is reported through the `e` row and lands as a note; an
                // answer with no rows at all means there was nothing to say about it.
                state.debug.frames.clear();
                state.debug.frame = 0;
            } else {
                if state.debug.frame >= frames.len() {
                    state.debug.frame = 0;
                }
                state.debug.frames = frames;
            }
        }

        DebugKind::Variables => {
            state.debug.variables = debug::parse_variables(&reply.payload);
        }

        DebugKind::Evaluate => {
            for entry in debug::parse_evaluate(&reply.payload) {
                state.debug.immediate.push(debug::immediate_line(&entry));
            }
            let over = state
                .debug
                .immediate
                .len()
                .saturating_sub(debug::MAX_IMMEDIATE);
            if over > 0 {
                state.debug.immediate.drain(0..over);
            }
        }

        DebugKind::Error => {
            // The payload is one `e` row holding the engine's own sentence. Unwrap it: the box
            // shows prose, not the record it travelled in.
            state.debug.note = debug::error_text(&reply.payload);
        }
    }
}

// ------------------------------------------------------------------ the controls

/// Writes what the model holds into the controls, leaving a control alone when its content has
/// not changed.
fn repaint(state: &mut state::AppState) {
    panel_status(state);
    panel_stack(state);
    panel_variables(state);
    panel_immediate(state);
}

fn panel_status(state: &mut state::AppState) {
    let text = match &state.debug.stop {
        Some(stop) => {
            let script = short_name(&stop.script);
            let script = if script.is_empty() {
                "the prompt".to_string()
            } else {
                script
            };
            // How many times this line has been reached is the answer to the question a
            // breakpoint that fires in a loop always raises.
            let hits = state
                .debug
                .breakpoints
                .iter()
                .find(|point| {
                    point.line == stop.line
                        && point
                            .script
                            .rsplit_once(['/', '\\'])
                            .map_or(&*point.script, |(_, name)| name)
                            == script
                })
                .map(|point| point.hits)
                .filter(|hits| *hits > 1)
                .map(|hits| format!(", hit {} times", hits))
                .unwrap_or_default();
            format!(
                "Stopped at line {} of {}{}{}",
                stop.line,
                script,
                if stop.reason == "breakpoint" {
                    ""
                } else {
                    " (stepping)"
                },
                hits
            )
        }
        // Not debugging is the truth whether or not a command is running, and saying more here
        // would only duplicate what the console pane is already showing.
        None => "Not debugging".to_string(),
    };
    if state.debugger.status_text != text {
        state.debugger.status_text = text.clone();
        state.debugger.status_label.set_text(&text);
    }

    // Both headings name the frame being read, because a Variables list that does not say
    // which frame it belongs to is a list of guesses once the call stack has more than one
    // frame in it.
    let heading = match state.debug.current_frame() {
        Some(_) => format!("Frame {} of {}", state.debug.frame + 1, state.debug.frames.len()),
        None => "Call stack".to_string(),
    };
    if state.debugger.stack_heading != heading {
        state.debugger.stack_heading = heading.clone();
        state.debugger.stack_label.set_text(&heading);
    }

    let vars_heading = match state.debug.current_frame() {
        Some(frame) => format!("Variables of {}", frame.function),
        None => "Variables".to_string(),
    };
    if state.debugger.vars_heading != vars_heading {
        state.debugger.vars_heading = vars_heading.clone();
        state.debugger.var_label.set_text(&vars_heading);
    }
}

fn panel_stack(state: &mut state::AppState) {
    let rows: Vec<String> = state
        .debug
        .frames
        .iter()
        .map(|frame| {
            let script = short_name(&frame.script);
            if script.is_empty() {
                format!("{}  line {}", frame.function, frame.line)
            } else {
                format!("{}  {} : {}", frame.function, script, frame.line)
            }
        })
        .collect();

    // A list box given the rows it already holds keeps neither its scroll position nor the
    // user's place in it, so the copy decides whether it is touched at all.
    if rows != state.debugger.stack_rows {
        state.debugger.stack_rows = rows;
        state
            .debugger
            .stack_list
            .set_collection(state.debugger.stack_rows.clone());
        // Read from the top. `set_collection` leaves the box showing whatever row it was last
        // looking at, so a deep stack from the previous stop would open the new one in the
        // middle of nowhere - and the frame execution is in is always the first row.
        scroll_to_top(&state.debugger.stack_list);
    }
    let chosen = if state.debug.frames.is_empty() {
        None
    } else {
        Some(state.debug.frame.min(state.debug.frames.len() - 1))
    };
    state.debugger.stack_list.set_selection(chosen);
}

fn panel_variables(state: &mut state::AppState) {
    let mut rows: Vec<String> = Vec::with_capacity(state.debug.variables.len());
    for row in &state.debug.variables {
        match row {
            VariableRow::Var(var) => {
                if var.value.is_empty() {
                    rows.push(format!("${} [{}]", var.name, var.type_name));
                } else {
                    rows.push(format!("${} [{}] {}", var.name, var.type_name, var.value));
                }
            }
            // A note takes the whole row: it is the engine talking about the frame rather than
            // something the frame holds. A failure is marked as one, so that a frame the window
            // could not read is not mistaken for a frame that has nothing in it.
            VariableRow::Note { error, text } => {
                rows.push(if *error {
                    format!("! {}", text)
                } else {
                    text.clone()
                })
            }
        }
    }
    if !state.debug.note.is_empty() {
        rows.push(state.debug.note.clone());
    }

    if rows != state.debugger.var_rows {
        state.debugger.var_rows = rows;
        state
            .debugger
            .var_list
            .set_collection(state.debugger.var_rows.clone());
        // From the top, and for a reason that only shows up on a long list: the frame of a script
        // running at the prompt holds the session's variables, which with a profile is three
        // hundred rows, and `set_collection` left the box at the row it had last been showing -
        // so the pane opened on the tail of the previous frame's list.
        scroll_to_top(&state.debugger.var_list);
    }
}

fn panel_immediate(state: &mut state::AppState) {
    // Joined with CRLF, not LF. This box is a plain Win32 edit control rather than the RichEdit
    // the two panes are, and a bare newline is a character to it, not a line break: the whole
    // transcript came out as one long line once the box stopped wrapping its own text, which is
    // how the exchange read as a single question with a nine word answer.
    let text = state.debug.immediate.join("\r\n");
    if state.debugger.immediate_text == text {
        return;
    }
    state.debugger.immediate_text = text.clone();
    state.debugger.immediate.set_text(&text);
    // The transcript grows at the bottom, so the newest line is the one worth looking at; a
    // box left scrolled to the top of sixty four answers is a box that shows nothing.
    state.debugger.immediate.scroll_lastline();
}

/// `LB_SETTOPINDEX`: the message that puts a chosen row at the top of a list box.
///
/// native-windows-gui gives its list boxes a selection but nothing to say where they are scrolled
/// to, and replacing the collection leaves the box wherever the rows it replaced had scrolled it.
/// That is invisible on five rows and wrong on three hundred, which is what a frame's variable
/// list is in a session with a profile: the pane opened showing the tail of the list.
const LB_SETTOPINDEX: u32 = 0x0197;

fn scroll_to_top(list: &nwg::ListBox<String>) {
    if let Some(hwnd) = list.handle.hwnd() {
        crate::console::send_message(hwnd as *mut std::ffi::c_void, LB_SETTOPINDEX, 0, 0);
    }
}

/// A script path in the length a column has: the file, not the folder it is in.
fn short_name(path: &str) -> String {
    match path.rsplit_once(['/', '\\']) {
        Some((_, name)) => name.to_string(),
        None => path.to_string(),
    }
}

// ------------------------------------------------------------------ requests

/// Evaluates what is in the expression box, and echoes the question into the transcript so the
/// answer can be read beside what it was an answer to.
pub fn evaluate_from_box() {
    let expression = state::read(|s| s.debugger.expr_box.text()).unwrap_or_default();
    if expression.trim().is_empty() {
        return;
    }
    evaluate(&expression);
    state::write(|s| s.debugger.expr_box.set_text(""));
}

/// Asks the stopped frame one question. Separated from the box above because the box is not the
/// only way to want this: the debug harness reaches it through `{EVAL:...}`, and a test that had
/// to type into a control to exercise a request would be testing the keyboard instead of the
/// debugger.
pub fn evaluate(expression: &str) {
    let expression = expression.trim().to_string();
    if expression.is_empty() {
        return;
    }

    state::write(|s| {
        s.debug.immediate.push(format!("? {}", expression));
        match s.engine.as_mut() {
            Some(engine) => engine.debug_request("p", &expression),
            None => s.debug.note = "There is no shell to ask.".to_string(),
        }
    });
}

/// Asks for the variables of the frame the Call Stack box is on.
pub fn read_selected_frame() {
    let depth = state::write(|s| {
        let chosen = s.debugger.stack_list.selection()?;
        s.debug.frame = chosen;
        s.debug.frames.get(chosen).map(|frame| frame.depth)
    })
    .flatten();

    let Some(depth) = depth else { return };
    state::write(|s| {
        if let Some(engine) = s.engine.as_mut() {
            engine.debug_request("v", &depth.to_string());
        }
    });
    state::write(|s| repaint(s));
}

/// Handles a notification from one of the column's controls. Gives back true when the event
/// belonged here, so the dispatcher can pass it on to the Commands column otherwise.
pub fn on_event(evt: nwg::Event, handle: &nwg::ControlHandle) -> bool {
    use nwg::Event as Evt;

    match evt {
        Evt::OnListBoxSelect => {
            if state::is(handle, |s| s.debugger.stack_list.handle) {
                read_selected_frame();
                true
            } else {
                // The Variables list has nothing to do when selected: a value is read, not
                // acted on, and copying it is what right-clicking the pane is for.
                false
            }
        }

        Evt::OnButtonClick => {
            if state::is(handle, |s| s.debugger.eval_button.handle) {
                evaluate_from_box();
                true
            } else {
                false
            }
        }

        _ => false,
    }
}
