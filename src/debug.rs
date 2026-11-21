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
            "l" => DebugKind::Variables,
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

fn rows(payload: &str) -> impl Iterator<Item = &str> {
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
    for row in rows(payload) {
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
    rows(payload)
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

/// Reads one frame's variables: the row marker, the name, the type, the value.
///
/// A row that starts with `e` is the engine saying the read itself failed, which is not the
/// same as a frame with nothing in it, so it is reported as a variable called nothing and
/// left for the pane to show as a line of its own.
pub fn parse_variables(payload: &str) -> (Vec<Variable>, Vec<String>) {
    let mut vars = Vec::new();
    let mut errors = Vec::new();
    for row in rows(payload) {
        let fields: Vec<&str> = row.split(FIELD).collect();
        match fields.first().copied().unwrap_or("") {
            "v" if fields.len() >= 4 => vars.push(Variable {
                name: field(&fields, 1),
                type_name: field(&fields, 2),
                value: fields[3..].join(&FIELD.to_string()),
            }),
            "e" => errors.push(field(&fields, 1)),
            _ => {}
        }
    }
    (vars, errors)
}

/// Reads the answer to an expression: `e` and a message, or `v`, a type and a value. The
/// value is joined from what is left, because a printed object can hold the field mark - the
/// engine replaces control characters, but not one it was handed inside a string it did not
/// write itself.
pub fn parse_evaluate(payload: &str) -> Vec<EvalRow> {
    rows(payload)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_comes_back_when_its_last_part_arrives() {
        let mut stream = DebugStream::default();
        assert!(stream.feed("l", 1, 3, 1, "one").is_none());
        assert!(stream.feed("l", 2, 3, 1, "two").is_none());
        let done = stream.feed("l", 3, 3, 1, "three").expect("the third part completes it");
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
        stream.feed("l", 1, 3, 2, "a");
        let done = stream.feed("l", 3, 3, 2, "c").expect("the gap is an answer of its own");
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
    fn variables_and_errors_are_told_apart() {
        let payload = format!(
            "v{f}sum{f}Int32{f}10{row}v{f}name{f}String{f}{row}e{f}no such scope",
            f = FIELD,
            row = ROW
        );
        let (vars, errors) = parse_variables(&payload);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name, "sum");
        assert_eq!(vars[0].type_name, "Int32");
        assert_eq!(vars[0].value, "10");
        assert_eq!(vars[1].value, "");
        assert_eq!(errors, vec!["no such scope".to_string()]);
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
