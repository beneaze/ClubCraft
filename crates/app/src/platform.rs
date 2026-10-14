//! Small OS-specific helpers (macOS / Windows / Linux).

use std::path::Path;

/// Show a file in Finder / Explorer.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let r = {
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path.as_os_str());
        std::process::Command::new("explorer").arg(arg).spawn()
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let r = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    if let Err(e) = r {
        log::warn!("reveal {}: {e}", path.display());
    }
}

/// Open a folder (or file) with the system default handler.
pub fn open(path: &Path) {
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let r = std::process::Command::new("xdg-open").arg(path).spawn();
    if let Err(e) = r {
        log::warn!("open {}: {e}", path.display());
    }
}

/// Local date-time for file names, e.g. `2026-10-14 21.05.33`.
pub fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H.%M.%S").to_string()
}

/// "Finder" / "Explorer" for menu labels.
pub fn file_manager() -> &'static str {
    if cfg!(target_os = "macos") {
        "Finder"
    } else if cfg!(target_os = "windows") {
        "Explorer"
    } else {
        "file manager"
    }
}
