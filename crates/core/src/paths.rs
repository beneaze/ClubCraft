use std::path::PathBuf;

/// Root of ClubCraft user data: `~/Music/ClubCraft` (override with `CLUBCRAFT_HOME`).
pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CLUBCRAFT_HOME") {
        return PathBuf::from(p);
    }
    dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Music")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ClubCraft")
}

pub fn library_db() -> PathBuf {
    data_dir().join("library.db")
}

/// Cached stem files (`<track hash>.stems.flac`, 8 channels).
pub fn stems_dir() -> PathBuf {
    data_dir().join("Stems")
}

pub fn waveform_dir() -> PathBuf {
    data_dir().join("Waveforms")
}

pub fn exports_dir() -> PathBuf {
    data_dir().join("Exports")
}

pub fn recordings_dir() -> PathBuf {
    data_dir().join("Recordings")
}

/// Location of separation models. Search order: `CLUBCRAFT_MODELS`, the app bundle
/// `Contents/Resources/models`, `<exe dir>/models`, then `models/` in the source checkout.
pub fn models_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CLUBCRAFT_MODELS") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut cands = Vec::new();
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("../Resources/models"));
            cands.push(dir.join("models"));
            for anc in dir.ancestors().take(5) {
                cands.push(anc.join("models"));
            }
        }
        for c in cands {
            if c.join("htdemucs.json").exists() || c.join("htdemucs_ft.json").exists() {
                return c;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models")
}

pub fn ensure_dirs() -> std::io::Result<()> {
    for d in [data_dir(), stems_dir(), waveform_dir(), exports_dir(), recordings_dir()] {
        std::fs::create_dir_all(d)?;
    }
    Ok(())
}
