use crate::protocol::{EventKind, OutputEvent};

#[derive(Clone, Debug)]
pub struct ColorRun {
    pub text: String,
    pub fg: [u8; 3],
}

#[derive(Clone, Debug, Default)]
pub struct ConsoleModel {
    pub runs: Vec<ColorRun>,
    pub plain: String,
    pub busy: bool,
    pub input_requested: bool,
    pub exit_code: Option<i32>,
    pub history: Vec<String>,
    pub history_index: Option<usize>,
    pub input: String,
    pub completions: Vec<String>,
    pub completion_index: Option<usize>,
    pub status: String,
}

impl ConsoleModel {
    pub fn new() -> Self {
        Self {
            status: "Starting...".to_string(),
            ..Default::default()
        }
    }

    pub fn push_event(&mut self, ev: OutputEvent) {
        match ev.kind {
            EventKind::CommandStarted => {
                self.busy = true;
                self.input_requested = false;
                self.status = "Running command...".to_string();
            }
            EventKind::CommandFinished => {
                self.busy = false;
                self.status = "Ready".to_string();
            }
            EventKind::InputRequested => {
                self.busy = false;
                self.input_requested = true;
                self.status = "Waiting for input...".to_string();
            }
            EventKind::Title => {
                // Title is handled by app; ignore in buffer.
            }
            EventKind::Progress => {
                self.push_text(&ev.text, ev.fg);
            }
            _ => {
                self.push_text(&ev.text, ev.fg);
            }
        }
    }

    pub fn push_text(&mut self, text: &str, fg: [u8; 3]) {
        if text.is_empty() {
            return;
        }
        self.plain.push_str(text);
        if let Some(last) = self.runs.last_mut() {
            if last.fg == fg {
                last.text.push_str(text);
                return;
            }
        }
        self.runs.push(ColorRun { text: text.to_string(), fg });
    }

    pub fn push_system(&mut self, text: &str) {
        self.push_text(text, [180, 180, 180]);
    }

    pub fn clear(&mut self) {
        self.runs.clear();
        self.plain.clear();
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() { return; }
        let idx = match self.history_index {
            None => self.history.len().saturating_sub(1),
            Some(i) => i.saturating_sub(1),
        };
        self.history_index = Some(idx);
        self.input = self.history[idx].clone();
    }

    pub fn history_down(&mut self) {
        let Some(idx) = self.history_index else { return; };
        if idx + 1 >= self.history.len() {
            self.history_index = None;
            self.input.clear();
        } else {
            let next = idx + 1;
            self.history_index = Some(next);
            self.input = self.history[next].clone();
        }
    }

    pub fn accept_completion(&mut self) {
        if let (Some(idx), true) = (self.completion_index, !self.completions.is_empty()) {
            let c = self.completions[idx % self.completions.len()].clone();
            self.input = c;
        }
    }

    pub fn next_completion(&mut self) {
        if self.completions.is_empty() { return; }
        let next = match self.completion_index {
            None => 0,
            Some(i) => (i + 1) % self.completions.len(),
        };
        self.completion_index = Some(next);
        self.input = self.completions[next].clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{EventKind, OutputEvent};

    fn ev(kind: EventKind, text: &str) -> OutputEvent {
        OutputEvent { kind, fg: [204,204,204], bg: [0,0,0], text: text.to_string() }
    }

    #[test]
    fn buffers_and_clears() {
        let mut m = ConsoleModel::new();
        m.push_text("hello", [1,2,3]);
        m.push_text(" world", [1,2,3]); // same color merges
        assert_eq!(m.runs.len(), 1);
        assert!(m.plain.contains("hello world"));
        m.clear();
        assert!(m.runs.is_empty());
        assert!(m.plain.is_empty());
    }

    #[test]
    fn history_navigation() {
        let mut m = ConsoleModel::new();
        m.history = vec!["one".into(), "two".into()];
        m.history_up();
        assert_eq!(m.input, "two");
        m.history_up();
        assert_eq!(m.input, "one");
        m.history_down();
        assert_eq!(m.input, "two");
        m.history_down();
        assert_eq!(m.input, "");
    }
}
