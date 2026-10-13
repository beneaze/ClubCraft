//! Stem file cache (placeholder).
use crate::Stems;
use anyhow::Result;
use clubcraft_core::StemTrack;
use std::path::Path;

pub fn save_stems(_path: &Path, _stems: &Stems) -> Result<()> { todo!() }
pub fn save_stem_track(_path: &Path, _t: &StemTrack) -> Result<()> { todo!() }
pub fn load_stems(_path: &Path) -> Result<Stems> { todo!() }
pub fn load_stem_track(_path: &Path) -> Result<StemTrack> { todo!() }
