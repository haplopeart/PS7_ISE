//! Engine protocol parser/encoder.
//!
//! This mirrors the existing PS7_ISE stdio protocol:
//! - Frontend -> engine: `L <text>`, `T <caret> <text>`, `M ...`, `S`, `P`, `X`
//! - Engine -> frontend: `E <kind> <fg> <bg> <text>`, `C <count> <text>`, `M ...`, `Q <code>`
//!
//! Text fields are escaped as printable ASCII: `\\` and `\uXXXX`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Output,
    Error,
    Warning,
    Verbose,
    Debug,
    Progress,
    Title,
    CommandStarted,
    CommandFinished,
    InputRequested,
    InputEcho,
    Verification,
    Unknown(i32),
}

impl EventKind {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Output,
            2 => Self::Error,
            3 => Self::Warning,
            4 => Self::Verbose,
            5 => Self::Debug,
            6 => Self::Progress,
            7 => Self::Title,
            8 => Self::CommandStarted,
            9 => Self::CommandFinished,
            10 => Self::InputRequested,
            11 => Self::InputEcho,
            12 => Self::Verification,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputEvent {
    pub kind: EventKind,
    pub fg: [u8; 3],
    pub bg: [u8; 3],
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataChunk {
    pub kind: String,
    pub part: i64,
    pub of: i64,
    pub reply: i64,
    pub payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebugStop {
    pub reason: String,
    pub line: i64,
    pub col: i64,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineRecord {
    Output(OutputEvent),
    Completions(Vec<String>),
    Metadata(MetadataChunk),
    DebugStop(DebugStop),
    DebugResumed,
    Quit(i32),
    Foreign(String),
}

pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for unit in text.encode_utf16() {
        match unit {
            0x5c => out.push_str("\\\\"),
            0x20..=0x7e => out.push(char::from(unit as u8)),
            other => out.push_str(&format!("\\u{:04x}", other)),
        }
    }
    out
}

pub fn decode(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
    let bytes = value.as_bytes();
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'\\' => {
                    units.push(b'\\' as u16);
                    i += 2;
                    continue;
                }
                b'u' => {
                    if let Some(hex) = value.get(i + 2..i + 6) {
                        if let Ok(code) = u16::from_str_radix(hex, 16) {
                            units.push(code);
                            i += 6;
                            continue;
                        }
                    }
                }
                _ => {}
            }
        }
        units.push(byte as u16);
        i += 1;
    }
    String::from_utf16_lossy(&units)
}

pub fn rgb(value: i64) -> [u8; 3] {
    let v = if value < 0 { 0 } else { value };
    [((v >> 16) & 0xff) as u8, ((v >> 8) & 0xff) as u8, (v & 0xff) as u8]
}

pub fn parse_record(record: &str) -> EngineRecord {
    let record = record.trim_end_matches(['\n', '\r']);
    if record.is_empty() {
        return EngineRecord::Foreign(String::new());
    }
    let mut fields = record.splitn(5, ' ');
    match fields.next() {
        Some("E") => {
            let kind = fields.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
            let fg = fields.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0xcc_cc_cc);
            let bg = fields.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let text = fields.next().unwrap_or("");
            EngineRecord::Output(OutputEvent {
                kind: EventKind::from_i32(kind),
                fg: rgb(fg),
                bg: rgb(bg),
                text: decode(text),
            })
        }
        Some("C") => {
            let _count = fields.next().and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
            let payload = fields.next().unwrap_or("");
            let items = decode(payload)
                .split('\u{1e}')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            EngineRecord::Completions(items)
        }
        Some("M") => {
            let mut meta = record.splitn(6, ' ');
            meta.next();
            let kind = meta.next().unwrap_or("").to_string();
            let part = meta.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let of = meta.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let reply = meta.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
            let payload = meta.next().unwrap_or("");
            EngineRecord::Metadata(MetadataChunk {
                kind,
                part,
                of,
                reply,
                payload: decode(payload),
            })
        }
        Some("Q") => {
            let code = fields.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
            EngineRecord::Quit(code)
        }
        Some("D") => {
            // D s <reason> <line> <col> <encodedPath...>  |  D r
            let mut d = record.splitn(6, ' ');
            d.next(); // D
            match d.next() {
                Some("r") => EngineRecord::DebugResumed,
                Some("s") => {
                    let reason = d.next().unwrap_or("").to_string();
                    let line = d.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                    let col = d.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
                    let path = d.next().unwrap_or("");
                    EngineRecord::DebugStop(DebugStop { reason, line, col, path: decode(path) })
                }
                _ => EngineRecord::Foreign(record.to_string()),
            }
        }
        _ => EngineRecord::Foreign(record.to_string()),
    }
}

// Frontend -> engine commands
pub fn cmd_submit_line(line: &str) -> String { format!("L {}\n", encode(line)) }
pub fn cmd_complete(caret: usize, text: &str) -> String { format!("T {} {}\n", caret, encode(text)) }
pub fn cmd_stop() -> String { "S\n".to_string() }
pub fn cmd_prompt() -> String { "P\n".to_string() }

pub fn cmd_debug_continue() -> String { "D c\n".to_string() }
pub fn cmd_debug_step_into() -> String { "D i\n".to_string() }
pub fn cmd_debug_step_over() -> String { "D o\n".to_string() }
pub fn cmd_debug_step_out() -> String { "D u\n".to_string() }
pub fn cmd_debug_stop() -> String { "D q\n".to_string() }
pub fn cmd_debug_stack() -> String { "D k\n".to_string() }
pub fn cmd_debug_breakpoints() -> String { "D b l\n".to_string() }
pub fn cmd_debug_breakpoint_add(line: i64, path: &str) -> String { format!("D b + {} {}\n", line, encode(path)) }
pub fn cmd_debug_breakpoint_remove(id: i64) -> String { format!("D b - {}\n", id) }
pub fn cmd_debug_breakpoint_enable(id: i64) -> String { format!("D b e {}\n", id) }
pub fn cmd_debug_breakpoint_disable(id: i64) -> String { format!("D b d {}\n", id) }
pub fn cmd_debug_breakpoint_clear() -> String { "D b c\n".to_string() }
pub fn cmd_exit() -> String { "X\n".to_string() }
pub fn cmd_meta_index() -> String { "M i\n".to_string() }
pub fn cmd_meta_refresh() -> String { "M r\n".to_string() }
pub fn cmd_meta_detail(name: &str) -> String { format!("M d {}\n", encode(name)) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_encode_decode() {
        let s = "hello \\u world \n\t café — ";
        assert_eq!(decode(&encode(s)), s);
    }

    #[test]
    fn parse_output_event() {
        let rec = parse_record("E 1 13421772 0 PS\\u003e\\u0020");
        match rec {
            EngineRecord::Output(ev) => {
                assert_eq!(ev.kind, EventKind::Output);
                assert_eq!(ev.fg, [204, 204, 204]);
                assert_eq!(ev.text, "PS> ");
            }
            _ => panic!("expected output"),
        }
    }

    #[test]
    fn parse_completions() {
        let rec = parse_record("C 2 Get-ChildItem\\u001eGet-Command");
        match rec {
            EngineRecord::Completions(items) => {
                assert_eq!(items, vec!["Get-ChildItem", "Get-Command"]);
            }
            _ => panic!("expected completions"),
        }
    }

    #[test]
    fn parse_quit() {
        assert_eq!(parse_record("Q 0"), EngineRecord::Quit(0));
    }

    #[test]
    fn debug_command_helpers() {
        assert_eq!(cmd_debug_continue(), "D c\n");
        assert_eq!(cmd_debug_step_over(), "D o\n");
        assert_eq!(cmd_debug_breakpoint_add(42, "C:\\script.ps1"), "D b + 42 C:\\\\script.ps1\n");
        assert_eq!(cmd_debug_breakpoint_remove(7), "D b - 7\n");
        assert_eq!(cmd_debug_breakpoint_enable(3), "D b e 3\n");
        assert_eq!(cmd_debug_breakpoint_disable(3), "D b d 3\n");
    }

    #[test]
    fn parse_debug_records() {
        match parse_record("D s breakpoint 42 1 C:\\script.ps1") {
            EngineRecord::DebugStop(s) => {
                assert_eq!(s.reason, "breakpoint");
                assert_eq!(s.line, 42);
                assert_eq!(s.col, 1);
                assert_eq!(s.path, "C:\\script.ps1");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(parse_record("D r"), EngineRecord::DebugResumed);
    }
}
