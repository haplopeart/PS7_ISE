use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub fn init() -> PathBuf {
    let path = std::env::temp_dir().join("ps7ise-crossplatform.log");
    LOG_PATH.get_or_init(|| path.clone());
    LOCK.get_or_init(|| Mutex::new(()));
    path
}

pub fn log(line: &str) {
    let _ = LOCK.get_or_init(|| Mutex::new(()));
    let path = LOG_PATH.get_or_init(|| std::env::temp_dir().join("ps7ise-crossplatform.log"));
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn init_returns_path() {
        let p = init();
        assert!(p.to_string_lossy().contains("ps7ise-crossplatform"));
    }
}
