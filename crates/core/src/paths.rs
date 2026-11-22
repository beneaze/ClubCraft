use std::path::{Path, PathBuf};

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

/// Where the in-app download installs the models: kept apart from the app so that an update
/// doesn't download them again. `<CLUBCRAFT_HOME>/Models` when that is set, otherwise the
/// platform's local data folder (`~/Library/Application Support/ClubCraft/Models`,
/// `%LOCALAPPDATA%\ClubCraft\Models`, `~/.local/share/ClubCraft/Models`).
pub fn user_models_dir() -> PathBuf {
    if std::env::var_os("CLUBCRAFT_HOME").is_some() {
        return data_dir().join("Models");
    }
    match dirs::data_local_dir() {
        Some(d) => d.join("ClubCraft").join("Models"),
        None => data_dir().join("Models"),
    }
}

/// Whether `dir` holds separation weights, not only the small JSON manifests that are in git.
pub fn has_models(dir: &Path) -> bool {
    let manifest = dir.join("htdemucs.json").exists() || dir.join("htdemucs_ft.json").exists();
    manifest
        && ["htdemucs", "htdemucs_ft_0"].iter().any(|m| {
            dir.join(format!("{m}.onnx")).exists() || dir.join(format!("{m}.mlmodelc")).exists()
        })
}

/// Location of separation models. Search order: `CLUBCRAFT_MODELS`, the downloaded models
/// ([`user_models_dir`]), the app bundle `Contents/Resources/models`, `<exe dir>/models`, then
/// `models/` in the source checkout. A folder with weights wins over one with only manifests.
pub fn models_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CLUBCRAFT_MODELS") {
        return PathBuf::from(p);
    }
    let mut cands = vec![user_models_dir()];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("../Resources/models"));
            cands.push(dir.join("models"));
            for anc in dir.ancestors().take(5) {
                cands.push(anc.join("models"));
            }
        }
    }
    if let Some(c) = cands.iter().find(|c| has_models(c)) {
        return c.clone();
    }
    if let Some(c) = cands.iter().find(|c| c.join("htdemucs.json").exists() || c.join("htdemucs_ft.json").exists()) {
        return c.clone();
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models")
}

/// Whether stem separation can run: [`models_dir`] has weights.
pub fn models_installed() -> bool {
    has_models(&models_dir())
}

pub fn ensure_dirs() -> std::io::Result<()> {
    for d in [data_dir(), stems_dir(), waveform_dir(), exports_dir(), recordings_dir()] {
        std::fs::create_dir_all(d)?;
    }
    Ok(())
}
