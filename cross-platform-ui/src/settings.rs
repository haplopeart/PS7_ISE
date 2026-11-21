use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub engine_path: Option<String>,
    pub font_size: f32,
    pub show_script_pane: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { engine_path: None, font_size: 14.0, show_script_pane: true }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        std::env::temp_dir().join("ps7ise-crossplatform-settings.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        std::fs::read_to_string(path).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_are_sane() {
        let s = Settings::default();
        assert!(s.font_size >= 10.0);
        assert!(s.show_script_pane);
    }
}
