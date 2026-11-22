//! The debugger's half of the front end: the engine's `D` records, stitched back together
//! and read into the things the Debug menu, the gutter and the Variables pane draw.
//!
//! The engine holds execution on the runspace thread when a breakpoint is hit, answers one
//! request at a time while it is held, and says so on the wire: `D s` when it stopped, the
//! answers, and `D r` when the stop ended. Everything here is about the answers - they are
//! chunked like a command index and arrive interleaved with console output, so they need a
//! stream of their own. See `PowerShellEngineHost\Program.cs` for the record format and
//! `DebugService.cs` for what each kind holds.

/// Separates one row of a payload from the next - the same mark the command index uses.
pub const ROW: char = '\u{1e}';
/// Separates the fields inside one row.
pub const FIELD: char = '\u{1f}';

/// Cap on one assembled answer, so a runaway `Get-Variable` over a huge array cannot eat
/// the process the way an unbounded string would.
const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

/// What a `D` answer is about. The letters are the engine's: see `DebugService::Handle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DebugKind {
    /// Every line breakpoint the session carries, with its id, place and state.
    Breakpoints,
    /// The frames execution is held between, innermost first.
    CallStack,
    /// The variables of one frame.
    Variables,
    /// The answer to an expression typed by the user.
    Evaluate,
    /// A refusal or a failure, in the engine's own words. Default, because a stream nobody
    /// has fed anything good to is not holding anything good.
    #[default]
    Error,
}

/// One complete answer, every chunk put back in order.
pub struct DebugReply {
    pub kind: DebugKind,
    pub payload: String,
}

/// A change in the stop, as opposed to an answer about it. These are events rather than
/// state because the UI reads them on a timer: a breakpoint hit and released between two
/// ticks still has to be seen, or the panes would keep showing a scope that is gone.
#[derive(Clone, Debug)]
pub enum DebugEvent {
    /// Execution is held at a breakpoint.
    Stopped(StopInfo),
    /// The stop ended - continued, stepped, or stopped for good.
    Resumed,
}

/// Where execution is held.
#[derive(Clone, Debug)]
pub struct StopInfo {
    /// What caused the stop. "breakpoint" today; the engine may learn other reasons later.
    pub reason: String,
    /// 1-based line, as PowerShell reports it.
    pub line: i32,
    /// 1-based column.
    pub column: i32,
    /// The script execution is held in, empty if the frame has no file of its own.
    pub script: String,
}

/// Puts the engine's chunked `D` records back together. Same job, and the same rules, as
/// [`crate::commands::MetadataStream`] - which is a separate object on purpose: the two
/// replies interleave by design, and one buffer cannot hold both.
#[derive(Default)]
pub struct DebugStream {
    /// The reply number being assembled, 0 when nothing is in hand. The engine's counter
    /// starts at 1, so 0 is not a number an answer can carry.
    reply: i64,
    kind: DebugKind,
    parts: String,
    got: i64,
    want: i64,
}

impl DebugStream {
    /// Takes one `D` record and returns the answer it completed, if it completed one.
    ///
    /// A part that does not follow on is not stitched in, exactly as in the metadata stream:
    /// a variables list with a hole in it looks like a scope that is missing names, which is
    /// a worse thing to tell a user than "that did not arrive, ask again".
    pub fn feed(&mut self, kind: &str, part: i64, of: i64, reply: i64, payload: &str) -> Option<DebugReply> {
        let kind = match kind {
            "b" => DebugKind::Breakpoints,
            "k" => DebugKind::CallStack,
            "v" => DebugKind::Variables,
            "p" => DebugKind::Evaluate,
            _ => DebugKind::Error,
        };
        let of = of.max(1);

        if self.reply != reply || part == 1 {
            self.reply = reply;
            self.kind = kind;
            self.parts.clear();
            self.got = 0;
            self.want = of;
        }

        if part != self.got + 1 {
            self.reset();
            return Some(error("The engine's debugger reply arrived out of order, so it was dropped."));
        }

        self.parts.push_str(payload);
        self.got = part;

        if self.parts.len() > MAX_PAYLOAD {
            self.reset();
            return Some(error("The engine's debugger reply was too large to read."));
        }

        if self.got < self.want {
            return None;
        }

        let kind = self.kind;
        let payload = std::mem::take(&mut self.parts);
        self.reply = 0;
        self.got = 0;
        self.want = 1;
        Some(DebugReply { kind, payload })
    }

    /// Drops whatever is in hand, because the engine is going away and the rest is not coming.
    pub fn reset(&mut self) {
        self.reply = 0;
        self.kind = DebugKind::Error;
        self.parts.clear();
        self.got = 0;
        self.want = 1;
    }
}

fn error(payload: &str) -> DebugReply {
    DebugReply {
        kind: DebugKind::Error,
        payload: payload.to_string(),
    }
}

/// One line breakpoint.
#[derive(Clone, Debug)]
pub struct Breakpoint {
    /// The engine's own number for it, which is what remove and enable ask for.
    pub id: i64,
    pub script: String,
    pub line: i32,
    /// A disabled breakpoint still shows in the gutter, dimmed, because forgetting that it is
    /// there is worse than seeing one that is off.
    pub enabled: bool,
    pub hits: i64,
}

/// One call stack frame.
#[derive(Clone, Debug)]
pub struct Frame {
    /// PowerShell's own scope number for this frame, which is what a variables request
    /// takes - so clicking a frame is all the pane has to do to read that frame's scope.
    pub depth: i64,
    pub function: String,
    pub script: String,
    pub line: i32,
}

/// One variable of the frame that is being looked at.
#[derive(Clone, Debug)]
pub struct Variable {
    pub name: String,
    /// The reported type, or `<null>` / `<unreadable>` when there was nothing to report.
    pub type_name: String,
    pub value: String,
}

/// One line of an evaluated expression's answer.
#[derive(Clone, Debug)]
pub struct EvalRow {
    pub error: bool,
    pub text: String,
}

fn rows_of(payload: &str) -> impl Iterator<Item = &str> {
    payload.split(ROW).filter(|row| !row.is_empty())
}

/// A field that may be missing, as the empty string rather than as a panic.
fn field(row: &[&str], index: usize) -> String {
    row.get(index).map(|value| value.to_string()).unwrap_or_default()
}

fn number(text: &str) -> i64 {
    text.trim().parse::<i64>().unwrap_or(0)
}

/// Reads the breakpoint list: id, script, line, enabled, hit count. A row that starts with
/// `e` is the engine saying the edit or the read failed, which is not the same as having no
/// breakpoints, so it comes back separately rather than as a line in the gutter.
pub fn parse_breakpoints(payload: &str) -> (Vec<Breakpoint>, Vec<String>) {
    let mut points = Vec::new();
    let mut errors = Vec::new();
    for row in rows_of(payload) {
        let fields: Vec<&str> = row.split(FIELD).collect();
        match fields.first().copied().unwrap_or("") {
            "e" => errors.push(field(&fields, 1)),
            "" => {}
            _ => points.push(Breakpoint {
                id: number(&field(&fields, 0)),
                script: field(&fields, 1),
                line: number(&field(&fields, 2)) as i32,
                enabled: field(&fields, 3) == "1",
                hits: number(&field(&fields, 4)),
            }),
        }
    }
    (points, errors)
}

/// Reads the call stack: depth, function, script, line.
pub fn parse_call_stack(payload: &str) -> Vec<Frame> {
    rows_of(payload)
        .map(|row| row.split(FIELD).collect::<Vec<_>>())
        .filter(|fields| fields.len() >= 4)
        .map(|fields| Frame {
            depth: number(&field(&fields, 0)),
            function: field(&fields, 1),
            script: field(&fields, 2),
            line: number(&field(&fields, 3)) as i32,
        })
        .collect()
}

/// One row of the Variables pane: a variable, or a line the engine wants shown instead of
/// the list.
#[derive(Clone, Debug)]
pub enum VariableRow {
    Var(Variable),
    /// The engine's own words. `error` is true for a failure it named and false for a note,
    /// of which there is one that matters: a frame that genuinely has no variables is not the
    /// same thing as a read that failed, and a pane that says "no variables" about the second
    /// one is lying to the person debugging.
    Note { error: bool, text: String },
}

/// Reads one frame's variables: the row marker, the name, the type, the value.
///
/// The markers are the engine's: `v` is a value, `e` is the read having failed, `n` is the
/// engine saying the frame is empty on purpose. A failure is not folded into the list, and a
/// note is not folded into a failure.
pub fn parse_variables(payload: &str) -> Vec<VariableRow> {
    let mut rows = Vec::new();
    for row in rows_of(payload) {
        let fields: Vec<&str> = row.split(FIELD).collect();
        match fields.first().copied().unwrap_or("") {
            "v" if fields.len() >= 4 => rows.push(VariableRow::Var(Variable {
                name: field(&fields, 1),
                type_name: field(&fields, 2),
                value: fields[3..].join(&FIELD.to_string()),
            })),
            "e" => rows.push(VariableRow::Note {
                error: true,
                text: field(&fields, 1),
            }),
            "n" => rows.push(VariableRow::Note {
                error: false,
                text: field(&fields, 1),
            }),
            _ => {}
        }
    }
    rows
}

/// Reads the answer to an expression: `e` and a message, or `v`, a type and a value. The
/// value is joined from what is left, because a printed object can hold the field mark - the
/// engine replaces control characters, but not one it was handed inside a string it did not
/// write itself.
pub fn parse_evaluate(payload: &str) -> Vec<EvalRow> {
    rows_of(payload)
        .map(|row| row.split(FIELD).collect::<Vec<_>>())
        .filter_map(|fields| match fields.first().copied().unwrap_or("") {
            "e" => Some(EvalRow { error: true, text: field(&fields, 1) }),
            "v" if fields.len() >= 3 => Some(EvalRow {
                error: false,
                text: format!("{}  {}", field(&fields, 1), fields[2..].join(&FIELD.to_string())),
            }),
            "v" => Some(EvalRow { error: false, text: field(&fields, 1) }),
            _ => None,
        })
        .collect()
}

/// What the window knows about a debugging session, in one place.
///
/// Every field is a *copy* of what the engine said, kept because the panes are painted from
/// the state and not from a conversation with the shell: a breakpoint list is drawn on every
/// caret move, and asking the engine each time would mean a round trip per keystroke for
/// information that only changes when the user edits one.
#[derive(Default)]
pub struct Model {
    /// Every line breakpoint the shell carries, as it last listed them. Empty is not the same
    /// as unknown: the list is asked for as soon as there is a shell to ask.
    pub breakpoints: Vec<Breakpoint>,
    /// The frames of the current stop, innermost first; empty while the shell is running.
    pub frames: Vec<Frame>,
    /// The variables of `frame`.
    pub variables: Vec<VariableRow>,
    /// Where execution is held, or `None` when it is not held.
    pub stop: Option<StopInfo>,
    /// Which entry of `frames` `variables` belongs to, so the Call Stack pane can mark it.
    pub frame: usize,
    /// The engine's words about the last request that failed, shown rather than swallowed.
    pub note: String,
    /// Answers to expressions, oldest first, shown in the Immediate box. Bounded, because a
    /// user who means to look later can also mean a hundred of them.
    pub immediate: Vec<String>,
    /// True once the shell has answered the breakpoint listing asked for when it started. It
    /// decides whether an empty list means "no breakpoints" or "not asked yet", and the gutter
    /// should not draw nothing for the second one as though it were the first.
    pub listed: bool,
}

impl Model {
    /// The breakpoint on this line of this script, which is what decides whether F9 sets one
    /// or removes one. Paths are compared the way Windows does, because the engine reports the
    /// full path it was given and the tab knows the path it saved under, and those differ in
    /// case as often as not.
    pub fn breakpoint_at(&self, script: &str, line: i32) -> Option<&Breakpoint> {
        self.breakpoints.iter().find(|point| {
            point.line == line
                && point.script.eq_ignore_ascii_case(script.trim_end_matches(['/', '\\']))
        })
    }

    /// The frame whose variables are being shown, for the pane's title line.
    pub fn current_frame(&self) -> Option<&Frame> {
        self.frames.get(self.frame)
    }

    /// Forgets the stop and everything read from it. The shell runs again: what a paused frame
    /// held a moment ago is not information about anything any more, and keeping it on screen
    /// is how a debugger gets believed when it is lying.
    pub fn clear_stop(&mut self) {
        self.frames.clear();
        self.variables.clear();
        self.stop = None;
        self.frame = 0;
    }
}

/// One line of the Immediate box: what came back from an expression. The question is already
/// in the transcript by the time this runs, so the answer is indented under it and the two
/// read as one exchange rather than as a list of unrelated lines.
pub fn immediate_line(entry: &EvalRow) -> String {
    if entry.error {
        format!("  ! {}", entry.text)
    } else {
        format!("  {}", entry.text)
    }
}

/// Pulls the sentence out of a failure answer. The engine writes a refusal as one `e` row
/// holding its own words, and the pane shows the words rather than the record shape they
/// travelled in.
pub fn error_text(payload: &str) -> String {
    let mut lines = Vec::new();
    for row in rows_of(payload) {
        let fields: Vec<&str> = row.split(FIELD).collect();
        match fields.first().copied().unwrap_or("") {
            "e" => lines.push(field(&fields, 1)),
            // A chunked refusal that arrived under some other kind still has rows worth
            // showing; a row with no marker at all is the engine's prose, which is the most
            // useful thing in the reply.
            _ if !row.is_empty() => lines.push(row.to_string()),
            _ => {}
        }
    }
    lines.join("\n")
}

/// How many answers to remember in the Immediate box.
pub const MAX_IMMEDIATE: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_comes_back_when_its_last_part_arrives() {
        let mut stream = DebugStream::default();
        assert!(stream.feed("v", 1, 3, 1, "one").is_none());
        assert!(stream.feed("v", 2, 3, 1, "two").is_none());
        let done = stream.feed("v", 3, 3, 1, "three").expect("the third part completes it");
        assert_eq!(done.kind, DebugKind::Variables);
        assert_eq!(done.payload, "onetwothree");
    }

    #[test]
    fn a_new_reply_replaces_one_nobody_is_waiting_for() {
        let mut stream = DebugStream::default();
        stream.feed("b", 1, 4, 7, "half");
        let done = stream.feed("k", 1, 1, 8, "whole").expect("a new answer of one part is done");
        assert_eq!(done.kind, DebugKind::CallStack);
        assert_eq!(done.payload, "whole");
    }

    #[test]
    fn a_hole_in_the_middle_is_reported_and_not_stitched() {
        let mut stream = DebugStream::default();
        stream.feed("v", 1, 3, 2, "a");
        let done = stream.feed("v", 3, 3, 2, "c").expect("the gap is an answer of its own");
        assert_eq!(done.kind, DebugKind::Error);
    }

    #[test]
    fn breakpoint_rows_round_trip() {
        // Exactly the shape DebugService writes: id, script, line, enabled, hits.
        let payload = format!("1{f}C:\\a\\b.ps1{f}12{f}1{f}3{row}2{f}C:\\c.ps1{f}7{f}0{f}0", f = FIELD, row = ROW);
        let (points, errors) = parse_breakpoints(&payload);
        assert!(errors.is_empty());
        assert_eq!(points.len(), 2);
        assert_eq!(points[0].id, 1);
        assert_eq!(points[0].line, 12);
        assert!(points[0].enabled);
        assert_eq!(points[0].hits, 3);
        assert!(!points[1].enabled);
    }

    #[test]
    fn a_failed_breakpoint_edit_is_not_mistaken_for_an_empty_list() {
        let payload = format!("e{f}Save the script before setting a breakpoint.", f = FIELD);
        let (points, errors) = parse_breakpoints(&payload);
        assert!(points.is_empty(), "a failure must not draw a breakpoint");
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn variables_and_notes_are_told_apart() {
        let payload = format!(
            "v{f}sum{f}Int32{f}10{row}v{f}name{f}String{f}{row}e{f}no such scope{row}n{f}nothing here",
            f = FIELD,
            row = ROW
        );
        let rows = parse_variables(&payload);
        assert_eq!(rows.len(), 4);
        match &rows[0] {
            VariableRow::Var(var) => {
                assert_eq!(var.name, "sum");
                assert_eq!(var.type_name, "Int32");
                assert_eq!(var.value, "10");
            }
            _ => panic!("the first row is a variable"),
        }
        match &rows[1] {
            VariableRow::Var(var) => assert_eq!(var.value, ""),
            _ => panic!("a variable with no value is still a variable"),
        }
        match &rows[2] {
            VariableRow::Note { error, text } => {
                assert!(error);
                assert_eq!(text, "no such scope");
            }
            _ => panic!("the failure is a note"),
        }
        match &rows[3] {
            VariableRow::Note { error, text } => {
                assert!(!error, "an empty frame is not a failure");
                assert_eq!(text, "nothing here");
            }
            _ => panic!("the engine's note is a note"),
        }
    }

    #[test]
    fn an_evaluate_answer_keeps_its_type_and_reports_failures() {
        let payload = format!(
            "v{f}Int32{f}42{row}e{f}The term 'nope' is not recognized.",
            f = FIELD,
            row = ROW
        );
        let rows = parse_evaluate(&payload);
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].error);
        assert_eq!(rows[0].text, "Int32  42");
        assert!(rows[1].error);
    }
}
