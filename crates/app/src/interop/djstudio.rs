//! Import from DJ.Studio: library tracks with their cues, and projects as ClubCraft mixes.
//! Read-only: DJ.Studio's files are never modified.
//!
//! Being re-implemented under a clean-room process: the previous importer was removed because
//! parts of it were derived from reading DJ.Studio's program code. The new one is written only
//! from observation (DJ.Studio's own data files and exports) and public documentation; see
//! `docs/interop/djstudio.md`.

use anyhow::{bail, Result};
use clubcraft_core::Library;
use std::path::{Path, PathBuf};

/// DJ.Studio's database folder in the user's music folder.
pub fn default_root() -> PathBuf {
    dirs::audio_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Music")).join("DJ.Studio").join("Database")
}

pub struct ImportReport {
    pub tracks_imported: usize,
    pub cues_imported: usize,
    pub missing_files: Vec<PathBuf>,
    pub mixes: Vec<(i64, String)>,
}

/// Import DJ.Studio's library and projects into `lib`.
pub fn import_all(_lib: &Library, _root: &Path) -> Result<ImportReport> {
    bail!("the DJ.Studio import is being rebuilt and is not available in this build")
}
