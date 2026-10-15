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
    /// "fast" | "high" | "ultra" | "best".
    pub quality: String,
    /// Use the All-In-One structure model (needs stems; ~1 min and +1.5 GB per track) for
    /// slightly better sections and cues. Off by default.
    pub deep_structure: bool,
    pub bpm_min: f64,
    pub bpm_max: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { separate_stems: true, quality: "best".into(), deep_structure: false, bpm_min: 70.0, bpm_max: 180.0 }
    }
}

impl Settings {
    pub fn quality(&self) -> Quality {
        Quality::parse(&self.quality).unwrap_or(Quality::Best)
    }
}

/// State a worker keeps between jobs (loaded models are expensive).
#[derive(Default)]
pub struct WorkerContext {
    separator: Option<(Quality, Separator)>,
}

impl WorkerContext {
    pub fn release(&mut self) {
        self.separator = None;
    }

    pub fn separator(&mut self, q: Quality) -> Result<&Separator> {
        if self.separator.as_ref().map(|s| s.0) != Some(q) {
            self.separator = None; // free the old models first
            let mut s = Separator::new(q).context("loading stem models")?;
            // Without GPU acceleration (CoreML / DirectML) the large models take many minutes per
            // track; fall back to the fast model.
            if s.backend_name() == "onnxruntime-cpu" && q != Quality::Fast {
                log::warn!("no GPU backend for stem separation; using the Fast model instead of {q:?}");
                drop(s);
                s = Separator::new(Quality::Fast).context("loading stem models")?;
            }
            log::info!("stem separator: {} on {}", s.model_name(), s.backend_name());
            self.separator = Some((q, s));
        }
        self.separator.as_ref().map(|(_, s)| s).ok_or_else(|| anyhow::anyhow!("no stem separator"))
    }
}

/// How good the stems of a model are (higher is better; 0 = unknown).
pub fn model_rank(model: &str) -> u8 {
    match model {
        "melroformer+htdemucs_ft" => 3,
        "htdemucs_ft" => 2,
        "htdemucs" | "htdemucs_6s" => 1,
        _ => 0,
    }
}

/// Whether stems from `model` are at least as good as `quality` ("fast" | "high" | "best" …).
pub fn stems_meet(model: Option<&str>, quality: &str) -> bool {
    let want = Quality::parse(quality).unwrap_or(Quality::Best).options().model;
    let want = if Quality::parse(quality) == Some(Quality::Best) { "melroformer+htdemucs_ft".to_string() } else { want };
    model.is_some_and(|m| model_rank(m) >= model_rank(&want))
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
    let want = if q == Quality::Best { "melroformer+htdemucs_ft".to_string() } else { q.options().model };
    let path = stems_path(&t.hash);
    if let Some(m) = cached_stems_model(&t.hash) {
        // Keep existing stems unless they came from a weaker model than requested.
        if model_rank(&m) >= model_rank(&want) {
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

/// Full analysis (uses the structure model when stems exist; can take ~20 s).
pub fn analyze(lib: &Library, id: TrackId, s: &Settings, progress: &mut dyn FnMut(f32, &str)) -> Result<()> {
    let src = if s.deep_structure { clubcraft_analysis::StructureSource::Auto } else { clubcraft_analysis::StructureSource::Rules };
    analyze_with(lib, id, s, src, progress)
}

/// Quick analysis (rule-based structure, ~1 s) for tracks loaded on a deck before analysis.
pub fn analyze_quick(lib: &Library, id: TrackId) -> Result<()> {
    let p = crate::prefs::Prefs::load();
    let s = Settings { bpm_min: p.bpm_min, bpm_max: p.bpm_max, ..Default::default() };
    analyze_with(lib, id, &s, clubcraft_analysis::StructureSource::Rules, &mut |_, _| {})
}

fn analyze_with(lib: &Library, id: TrackId, s: &Settings, structure: clubcraft_analysis::StructureSource, progress: &mut dyn FnMut(f32, &str)) -> Result<()> {
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
    let opts = AnalyzeOptions {
        stems_model: stems.as_ref().map(|s| s.model.clone()),
        structure,
        beat: clubcraft_analysis::BeatOptions::with_range(s.bpm_min, s.bpm_max),
        ..Default::default()
    };
    let sref = owned.as_ref().map(|o| o.as_ref());
    let (analysis, wf) = rayon::join(|| clubcraft_analysis::analyze(&audio, sref.as_ref(), &opts), || clubcraft_analysis::waveform(&audio, sref));
    progress(0.9, "Saving");
    wf.save(&t.hash)?;
    lib.save_analysis(id, &analysis)?;
    if let (Some(sr), Some(grid)) = (sref.as_ref(), analysis.grid) {
        progress(0.95, "Mashup features");
        let f = mashup_features_from(sr, grid, &analysis);
        if let Err(e) = save_mashup_features(&t.hash, &f) {
            log::warn!("mashup features: {e:#}");
        }
    }
    let _ = Waveform::cache_path(&t.hash);
    Ok(())
}

// ------------------------------------------------------------------ mashup features

pub fn mashup_features_path(hash: &str) -> std::path::PathBuf {
    clubcraft_core::paths::data_dir().join("Mashup").join(format!("{hash}.ccmf"))
}

fn mashup_features_from(stems: &clubcraft_analysis::StemsRef, grid: clubcraft_core::BeatGrid, a: &clubcraft_core::Analysis) -> clubcraft_analysis::mashup::MashupFeatures {
    let starts: Vec<f64> = a.sections.iter().map(|s| s.start).collect();
    clubcraft_analysis::mashup::compute(stems, grid, a.duration, a.gain_db.unwrap_or(0.0), &starts)
}

fn save_mashup_features(hash: &str, f: &clubcraft_analysis::mashup::MashupFeatures) -> Result<()> {
    let p = mashup_features_path(hash);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, f.to_bytes())?;
    Ok(())
}

/// Cached mashup features of a track if they are current (same grid as the analysis).
pub fn cached_mashup_features(hash: &str, a: &clubcraft_core::Analysis) -> Option<clubcraft_analysis::mashup::MashupFeatures> {
    let f = clubcraft_analysis::mashup::MashupFeatures::from_bytes(&std::fs::read(mashup_features_path(hash)).ok()?)?;
    let g = a.grid?;
    (f.version == clubcraft_analysis::mashup::FEATURE_VERSION && (f.bpm - g.bpm).abs() < 1e-6 && (f.downbeat - g.downbeat).abs() < 1e-6).then_some(f)
}

/// Mashup features of a track, computed from its cached stems when missing or stale (~1-2 s,
/// a few hundred MB while it runs; callers hold the heavy-work lock). `None` without stems.
pub fn mashup_features(lib: &Library, id: TrackId) -> Result<Option<clubcraft_analysis::mashup::MashupFeatures>> {
    let t = lib.track(id)?.context("unknown track")?;
    let Some(a) = lib.analysis(id)? else { return Ok(None) };
    if let Some(f) = cached_mashup_features(&t.hash, &a) {
        return Ok(Some(f));
    }
    let Some(grid) = a.grid else { return Ok(None) };
    let sp = stems_path(&t.hash);
    if !sp.exists() {
        return Ok(None);
    }
    let st = clubcraft_stems::load_stem_track(&sp)?;
    let owned = OwnedStems::from_stem_track_partial(&st);
    drop(st);
    let f = mashup_features_from(&owned.as_ref(), grid, &a);
    save_mashup_features(&t.hash, &f)?;
    Ok(Some(f))
}
