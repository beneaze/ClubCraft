//! Per-track processing: stem separation and analysis. Runs inside job workers.

use anyhow::{bail, Context, Result};
use clubcraft_analysis::{AnalyzeOptions, OwnedStems};
use clubcraft_core::library::stems_path;
use clubcraft_core::{Library, TrackId, Waveform};
use clubcraft_stems::{Quality, Separator};
use std::sync::atomic::{AtomicBool, Ordering};

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

impl Settings {
    pub fn quality(&self) -> Quality {
        Quality::parse(&self.quality).unwrap_or(Quality::High)
    }
}

/// State a worker keeps between jobs (loaded models are expensive).
#[derive(Default)]
pub struct WorkerContext {
    separator: Option<(Quality, Separator)>,
}

impl WorkerContext {
    pub fn separator(&mut self, q: Quality) -> Result<&Separator> {
        if self.separator.as_ref().map(|s| s.0) != Some(q) {
            self.separator = None; // free the old models first
            let s = Separator::new(q).context("loading stem models")?;
            log::info!("stem separator: {} on {}", s.model_name(), s.backend_name());
            self.separator = Some((q, s));
        }
        Ok(&self.separator.as_ref().unwrap().1)
    }
}

/// Model name stored with the cached stems of a track, if any.
pub fn cached_stems_model(hash: &str) -> Option<String> {
    let p = stems_path(hash);
    if !p.exists() {
        return None;
    }
    clubcraft_stems::cache::read_model_name(&p).ok().flatten().or(Some("unknown".into()))
}

pub fn separate(
    lib: &Library,
    ctx: &mut WorkerContext,
    id: TrackId,
    s: &Settings,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<()> {
    let t = lib.track(id)?.context("unknown track")?;
    let q = s.quality();
    let want = q.options().model;
    let path = stems_path(&t.hash);
    if let Some(m) = cached_stems_model(&t.hash) {
        // Keep existing stems unless they came from a weaker model than requested.
        let rank = |m: &str| match m {
            "htdemucs_ft" => 2,
            "htdemucs" | "htdemucs_6s" => 1,
            _ => 0,
        };
        if rank(&m) >= rank(&want) {
            return Ok(());
        }
    }
    progress(0.0, "Decoding");
    let audio = clubcraft_audio::decode_file(&t.path, clubcraft_core::SAMPLE_RATE)?;
    progress(0.0, "Loading models");
    let sep = ctx.separator(q)?;
    let label = format!("Stems ({})", sep.model_name());
    let stems = sep.separate(&audio, &mut |p| {
        progress(p, &label);
        !cancel.load(Ordering::Relaxed)
    })?;
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }
    progress(1.0, "Saving stems");
    clubcraft_stems::save_stems_with_model(&path, &stems, sep.model_name())?;
    Ok(())
}

pub fn analyze(lib: &Library, id: TrackId, _s: &Settings, progress: &mut dyn FnMut(f32, &str)) -> Result<()> {
    let t = lib.track(id)?.context("unknown track")?;
    progress(0.1, "Decoding");
    let audio = clubcraft_audio::decode_file(&t.path, clubcraft_core::SAMPLE_RATE)?;
    let stems_file = stems_path(&t.hash);
    let stems = if stems_file.exists() {
        progress(0.3, "Loading stems");
        match clubcraft_stems::load_stem_track(&stems_file) {
            Ok(st) if st.frames.abs_diff(audio.frames()) < 4096 => Some(st),
            Ok(_) => {
                log::warn!("stem file length mismatch for {}, ignoring", t.path.display());
                None
            }
            Err(e) => {
                log::warn!("stems for {}: {e:#}", t.path.display());
                None
            }
        }
    } else {
        None
    };
    progress(0.5, "Analysing");
    let owned = stems.as_ref().map(OwnedStems::from_stem_track_partial);
    let opts = AnalyzeOptions { stems_model: stems.as_ref().map(|s| s.model.clone()), ..Default::default() };
    let sref = owned.as_ref().map(|o| o.as_ref());
    let (analysis, wf) = rayon::join(|| clubcraft_analysis::analyze(&audio, sref.as_ref(), &opts), || clubcraft_analysis::waveform(&audio, sref));
    progress(0.9, "Saving");
    wf.save(&t.hash)?;
    lib.save_analysis(id, &analysis)?;
    let _ = Waveform::cache_path(&t.hash);
    Ok(())
}
