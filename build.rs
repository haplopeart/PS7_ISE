//! Compiles the Windows resources into `PS7_ISE.exe`: the application icon and the
//! version info that Explorer's Properties sheet shows.
//!
//! The icon is `assets\ps7ise.ico`, the PowerShell 7 logo taken out of `pwsh.exe`, and it
//! is deliberately not re-encoded - the file carries 9 sizes from 16 px up to 256 px so
//! Explorer, the taskbar, Alt+Tab and the title bar each get one that fits. `src\icons.rs`
//! loads that same resource back out at run time, so the window icon and the file icon are
//! the same pixels rather than two copies that can drift apart.
//!
//! Resources need the SDK's `rc.exe`. It is not normally on `PATH`, so it is located here
//! and handed over through `WINRC`, together with the `INCLUDE` paths `rc.exe` needs for
//! `windows.h`. Without a Windows SDK the build still succeeds and simply produces an exe
//! with the default icon - a Linux cross-compile or a bare CI box should not fail.

use std::path::{Path, PathBuf};

/// The highest `10.0.xxxx.0` directory under `dir`, newest last.
fn version_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .filter(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().starts_with("10.0."))
                    .unwrap_or(false)
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    found.sort();
    found
}

/// Windows SDK roots, newest first.
fn sdk_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for key in ["PROGRAMFILES", "ProgramFiles", "ProgramFiles(x86)", "PROGRAMFILES(X86)"] {
        if let Some(value) = std::env::var_os(key) {
            let root = PathBuf::from(value).join("Windows Kits").join("10");
            if root.is_dir() && !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    roots
}

/// `rc.exe` and the include tree that goes with it.
fn resource_compiler() -> Option<(PathBuf, Vec<PathBuf>)> {
    for root in sdk_roots() {
        for version in version_dirs(&root.join("bin")).iter().rev() {
            for arch in ["x64", "x86"] {
                let rc = version.join(arch).join("rc.exe");
                if !rc.is_file() {
                    continue;
                }
                let include = root.join("Include").join(version.file_name()?);
                let mut paths = Vec::new();
                for part in ["ucrt", "um", "shared", "winrt", "cppwinrt"] {
                    let path = include.join(part);
                    if path.is_dir() {
                        paths.push(path);
                    }
                }
                if !paths.is_empty() {
                    return Some((rc, paths));
                }
            }
        }
    }
    None
}

fn main() {
    println!("cargo:rerun-if-changed=assets/ps7ise.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var_os("WINRC").is_none() {
        let Some((rc, includes)) = resource_compiler() else {
            println!(
                "cargo:warning=no rc.exe in the Windows SDK: {} will be built without its \
                 icon and version info",
                env!("CARGO_PKG_NAME")
            );
            return;
        };
        std::env::set_var("WINRC", &rc);
        if std::env::var_os("INCLUDE").is_none() {
            std::env::set_var("INCLUDE", std::env::join_paths(&includes).unwrap_or_default());
        }
    }

    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/ps7ise.ico");
    resource.set("ProductName", "Windows PowerShell 7 ISE");
    resource.set("FileDescription", "Windows PowerShell 7 ISE");
    resource.set("CompanyName", "haplopeart");
    resource.set("LegalCopyright", "MIT License");
    resource.set("OriginalFilename", "PS7_ISE.exe");
    resource.set("InternalName", "PS7_ISE.exe");
    // The manifest exists to ask for one thing: version 6 of the common controls. Without it
    // Windows gives the process the classic v5 controls, which are drawn by `user32` from system
    // colours and ignore the dark app mode entirely - a window whose panes go dark and whose
    // buttons, tab strip and status bar stay grey. Asking for v6 puts those controls in the hands
    // of comctl32, which is the library that knows how to draw them dark (see `src/theme.rs`).
    //
    // It deliberately says nothing about DPI awareness. The fonts and layout constants in this
    // program are measured in device units, and becoming DPI-aware without reworking every one of
    // them would shrink the text and crowd the panes on a scaled display - a separate change, with
    // its own testing, not something to slip in beside a theme.
    // Write the manifest to a file and reference it from the resource script instead of
    // embedding the XML inline: `rc.exe` on this machine's Windows SDK rejects inline
    // manifest XML with `RC2021`, while reading it from a file works fine.
    let manifest_path = std::env::temp_dir().join(format!("{}.manifest", env!("CARGO_PKG_NAME")));
    let manifest_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
                        version="6.0.0.0" processorArchitecture="*"
                        publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and 11. The dark app mode is a Windows 10 feature, and an application that
           does not declare itself compatible is not offered the newer behaviour. -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>"#;
    std::fs::write(&manifest_path, manifest_xml).expect("Could not write manifest file");
    resource.set_manifest_file(manifest_path.to_str().unwrap());
    resource.compile().expect("Could not compile the Windows resources");
}
