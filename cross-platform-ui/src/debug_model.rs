#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BreakpointInfo {
    pub id: i64,
    pub script: String,
    pub line: i64,
    pub enabled: bool,
    pub hit_count: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StackFrameInfo {
    pub index: i64,
    pub function: String,
    pub script: String,
    pub line: i64,
}

#[derive(Clone, Debug, Default)]
pub struct DebugModel {
    pub breakpoints: Vec<BreakpointInfo>,
    pub call_stack: Vec<StackFrameInfo>,
    pub last_stop: Option<String>,
    pub new_bp_line: String,
    pub new_bp_path: String,
}

impl DebugModel {
    pub fn load_breakpoints(&mut self, payload: &str) {
        let mut out = Vec::new();
        for row in payload.split('\u{1e}') {
            if row.trim().is_empty() { continue; }
            let f: Vec<&str> = row.split('\u{1f}').collect();
            // Error row: RowOf("0","", "0", "message") or RowOf("0","","0","msg") - id=0,script="",line=0,...
            if f.len() >= 4 && f[0] == "0" && f.get(3).map(|s| *s != "0" && *s != "1").unwrap_or(false) {
                // likely error message row; skip as breakpoint
                continue;
            }
            if f.len() < 4 { continue; }
            let id = f[0].parse().unwrap_or(0);
            let script = f.get(1).unwrap_or(&"").to_string();
            let line = f.get(2).unwrap_or(&"0").parse().unwrap_or(0);
            let enabled = *f.get(3).unwrap_or(&"0") == "1";
            let hit_count = f.get(4).unwrap_or(&"0").parse().unwrap_or(0);
            out.push(BreakpointInfo { id, script, line, enabled, hit_count });
        }
        self.breakpoints = out;
    }

    pub fn load_call_stack(&mut self, payload: &str) {
        let mut out = Vec::new();
        for row in payload.split('\u{1e}') {
            if row.trim().is_empty() { continue; }
            let f: Vec<&str> = row.split('\u{1f}').collect();
            if f.len() < 4 { continue; }
            if f[0] == "e" { continue; }
            let index = f[0].parse().unwrap_or(0);
            let function = f.get(1).unwrap_or(&"").to_string();
            let script = f.get(2).unwrap_or(&"").to_string();
            let line = f.get(3).unwrap_or(&"0").parse().unwrap_or(0);
            out.push(StackFrameInfo { index, function, script, line });
        }
        self.call_stack = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_breakpoints() {
        let mut m = DebugModel::default();
        m.load_breakpoints("1\u{1f}C:\\\\script.ps1\u{1f}42\u{1f}1\u{1f}0\u{1e}2\u{1f}C:\\\\other.ps1\u{1f}10\u{1f}0\u{1f}3");
        assert_eq!(m.breakpoints.len(), 2);
        assert_eq!(m.breakpoints[0].id, 1);
        assert_eq!(m.breakpoints[0].line, 42);
        assert!(m.breakpoints[0].enabled);
        assert!(!m.breakpoints[1].enabled);
        assert_eq!(m.breakpoints[1].hit_count, 3);
    }

    #[test]
    fn parses_call_stack() {
        let mut m = DebugModel::default();
        m.load_call_stack("0\u{1f}Invoke-Foo\u{1f}C:\\\\script.ps1\u{1f}12\u{1e}1\u{1f}<script>\u{1f}\u{1f}1");
        assert_eq!(m.call_stack.len(), 2);
        assert_eq!(m.call_stack[0].function, "Invoke-Foo");
        assert_eq!(m.call_stack[0].line, 12);
    }
}
