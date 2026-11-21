use std::path::PathBuf;

/// Platform abstraction layer - keep OS-specific behavior here.
pub fn default_log_dir() -> PathBuf {
    std::env::temp_dir().join("ps7ise-crossplatform")
}

pub fn engine_candidate_paths(repo_root: Option<&std::path::Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(root) = repo_root {
        paths.push(root.join("dist").join("PowerShellEngineHost.exe"));
        paths.push(root.join("dist").join("PowerShellEngineHost"));
        paths.push(root.join("PowerShellEngineHost").join("PowerShellEngineHost").join("bin").join("Debug").join("net10.0").join("PowerShellEngineHost"));
        paths.push(root.join("PowerShellEngineHost").join("PowerShellEngineHost").join("bin").join("Release").join("net10.0").join("PowerShellEngineHost"));
    }
    paths
}

pub fn platform_name() -> &'static str {
    #[cfg(target_os = "macos")]
    { "macOS" }
    #[cfg(target_os = "linux")]
    { "Linux" }
    #[cfg(target_os = "windows")]
    { "Windows" }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    { "unknown" }
}
