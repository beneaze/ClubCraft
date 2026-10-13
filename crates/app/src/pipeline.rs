//! Per-track processing: stem separation and analysis. Runs inside job workers.

use anyhow::{Context, Result};
use clubcraft_core::{Analysis, Library, TrackId};

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Separate stems for every analysed track (cached on disk).
    pub separate_stems: bool,
    /// "fast" | "high" | "ultra".
    pub quality: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { separate_stems: true, quality: "high".into() }
    }
}

/// State a worker keeps between jobs (loaded models).
#[derive(Default)]
pub struct WorkerContext {}

pub fn separate(_lib: &Library, _ctx: &mut WorkerContext, _id: TrackId, _s: &Settings, _progress: &mut dyn FnMut(f32, &str)) -> Result<()> {
    Ok(())
}

pub fn analyze(lib: &Library, id: TrackId, _s: &Settings, progress: &mut dyn FnMut(f32, &str)) -> Result<()> {
    let t = lib.track(id)?.context("unknown track")?;
    progress(0.1, "Decoding");
    let audio = clubcraft_audio::decode_file(&t.path, clubcraft_core::SAMPLE_RATE)?;
    let a = Analysis { version: 0, duration: audio.duration(), ..Default::default() };
    lib.save_analysis(id, &a)?;
    Ok(())
}
