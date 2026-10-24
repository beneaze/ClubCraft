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

/// Model name of the stems a quality makes.
pub fn quality_model(q: Quality) -> String {
    if q == Quality::Best {
        "melroformer+htdemucs_ft".to_string()
    } else {
        q.options().model
    }
}

/// The quality that makes stems of `model` (the cheapest one, for `htdemucs_ft`).
fn model_quality(model: &str) -> Option<Quality> {
    match model {
        "melroformer+htdemucs_ft" => Some(Quality::Best),
        "htdemucs_ft" => Some(Quality::High),
        "htdemucs" => Some(Quality::Fast),
        _ => None,
    }
}

/// Whether stems from `model` are at least as good as `quality` ("fast" | "high" | "best" …).
pub fn stems_meet(model: Option<&str>, quality: &str) -> bool {
    let want = quality_model(Quality::parse(quality).unwrap_or(Quality::Best));
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
    let want = quality_model(q);
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
    // complete stems replace the parts separated for mix playback, unless those are better (an
    // export's transitions in Best, the whole track in the playback quality)
    let regions = clubcraft_core::library::region_stems_path(&t.hash);
    let region_model = clubcraft_stems::read_model_name(&regions).ok().flatten();
    if region_model.is_none_or(|m| model_rank(&m) <= model_rank(sep.model_name())) {
        let _ = std::fs::remove_file(regions);
    }
    Ok(())
}

/// Margin around each region (s): the stretcher reads ahead, and curves are edited at the edges.
const REGION_MARGIN: f64 = 3.0;

/// Stems for parts of a track only (`regions` in track seconds): what its stem transitions need.
/// `quality` `None` is for mix playback: the Fast quality, ready in seconds instead of minutes,
/// and nothing to do when the track has complete stems. `Some` is for an export: only the
/// transitions are separated in the export quality, not whole tracks (a Best export of a mix
/// with 16-bar transitions separates a fraction of each track), and complete stems are only
/// enough when they are that good. Added to the track's partial stem file
/// ([`clubcraft_core::library::region_stems_path`]); never replaced by weaker stems, so a
/// playback request on a track with Best parts separates in Best. Returns whether new stems
/// were saved.
pub fn separate_regions(
    lib: &Library,
    ctx: &mut WorkerContext,
    id: TrackId,
    regions: &[(f64, f64)],
    quality: Option<Quality>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> Result<bool> {
    use clubcraft_core::STEM_CHUNK_FRAMES as CHUNK;
    let t = lib.track(id)?.context("unknown track")?;
    let q = quality.unwrap_or(Quality::Fast);
    if let Some(m) = cached_stems_model(&t.hash) {
        if quality.is_none() || model_rank(&m) >= model_rank(&quality_model(q)) {
            return Ok(false);
        }
    }
    let path = clubcraft_core::library::region_stems_path(&t.hash);
    let earlier = clubcraft_stems::read_model_name(&path).ok().flatten();
    let q = match earlier.as_deref().and_then(model_quality) {
        Some(e) if model_rank(&quality_model(e)) > model_rank(&quality_model(q)) => e,
        _ => q,
    };
    progress(0.0, "Decoding");
    let audio = clubcraft_audio::decode_file(&t.path, clubcraft_core::SAMPLE_RATE)?;
    let n = audio.frames();
    if n == 0 {
        return Ok(false);
    }
    let last = (n - 1) / CHUNK;
    let chunk_of = |sec: f64| ((sec.max(0.0) * clubcraft_core::SAMPLE_RATE as f64) as usize / CHUNK).min(last);
    let wanted: Vec<(usize, usize)> = regions.iter().map(|&(a, b)| (chunk_of(a - REGION_MARGIN), chunk_of(b + REGION_MARGIN))).filter(|(a, b)| a <= b).collect();
    progress(0.0, "Loading models");
    let sep = ctx.separator(q)?;
    let model = sep.model_name().to_string();
    // no GPU, so the Fast model: it must not replace better parts made elsewhere
    if earlier.as_deref().is_some_and(|e| model_rank(e) > model_rank(&model)) {
        return Ok(false);
    }
    // extend an earlier partial file of the same model
    let st = match path.exists().then(|| clubcraft_stems::load_stem_track(&path).ok()).flatten() {
        Some(e) if e.frames == n && e.model == model => e,
        _ => clubcraft_core::StemTrack::new(n, model.clone()),
    };
    let todo: Vec<(usize, usize)> = wanted.into_iter().filter(|&(a, b)| !(a..=b).all(|c| st.is_chunk_ready(c))).collect();
    if todo.is_empty() {
        return Ok(false);
    }
    let total: usize = todo.iter().map(|(a, b)| b - a + 1).sum();
    let label = format!("Stems for transitions ({model})");
    for &(c0, c1) in &todo {
        // runs from the region's start onwards; stopped as soon as the region is covered
        let stop = AtomicBool::new(false);
        let r = sep.separate_into(
            &audio,
            &st,
            c0 * CHUNK,
            &mut |_| {
                let done: usize = todo.iter().map(|&(a, b)| (a..=b).filter(|&c| st.is_chunk_ready(c)).count()).sum();
                progress(done as f32 / total as f32, &label);
                if (c0..=c1).all(|c| st.is_chunk_ready(c)) || cancel.load(Ordering::Relaxed) {
                    stop.store(true, Ordering::Relaxed);
                }
            },
            &stop,
        );
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        if let Err(e) = r {
            if !e.is::<clubcraft_stems::Cancelled>() {
                return Err(e);
            }
        }
    }
    progress(1.0, "Saving stems");
    clubcraft_stems::save_partial_stem_track(&path, &st, &model)?;
    Ok(true)
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
    // the structure model runs here rather than inside the analysis, so its labels can be kept
    let act = if structure == clubcraft_analysis::StructureSource::Auto { sref.as_ref().and_then(clubcraft_analysis::allinone::shared_activations) } else { None };
    let (analysis, wf) = rayon::join(|| clubcraft_analysis::analyze_with_activations(&audio, sref.as_ref(), &opts, act.as_ref()), || clubcraft_analysis::waveform(&audio, sref));
    progress(0.9, "Saving");
    wf.save(&t.hash)?;
    lib.save_analysis(id, &analysis)?;
    if let (Some(sr), Some(grid)) = (sref.as_ref(), analysis.grid) {
        progress(0.95, "Mashup features");
        let f = mashup_features_from(sr, grid, &analysis);
        if let Err(e) = save_mashup_features(&t.hash, &f) {
            log::warn!("mashup features: {e:#}");
        }
        let m = clubcraft_analysis::vocals::compute(sr, grid, analysis.duration);
        if let Err(e) = save_vocal_map(&t.hash, &m) {
            log::warn!("vocal map: {e:#}");
        }
        if let Some(act) = &act {
            if let Err(e) = save_labels(&t.hash, &clubcraft_analysis::labels::BarLabels::from_activations(act, grid, analysis.duration)) {
                log::warn!("structure labels: {e:#}");
            }
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
    let mut f = clubcraft_analysis::mashup::compute(stems, grid, a.duration, a.gain_db.unwrap_or(0.0), &starts);
    f.set_sections(&a.sections);
    f
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
    let mut f = clubcraft_analysis::mashup::MashupFeatures::from_bytes(&std::fs::read(mashup_features_path(hash)).ok()?)?;
    let g = a.grid?;
    if f.version != clubcraft_analysis::mashup::FEATURE_VERSION || (f.bpm - g.bpm).abs() > 1e-6 || (f.downbeat - g.downbeat).abs() > 1e-6 {
        return None;
    }
    f.set_sections(&a.sections);
    Some(f)
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

// ------------------------------------------------------------------ vocal maps

pub fn vocal_map_path(hash: &str) -> std::path::PathBuf {
    clubcraft_core::paths::data_dir().join("Mashup").join(format!("{hash}.ccvm"))
}

fn save_vocal_map(hash: &str, m: &clubcraft_analysis::vocals::VocalMap) -> Result<()> {
    let p = vocal_map_path(hash);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, m.to_bytes())?;
    Ok(())
}

/// Cached vocal map of a track if it is current (same grid as the analysis).
pub fn cached_vocal_map(hash: &str, a: &clubcraft_core::Analysis) -> Option<clubcraft_analysis::vocals::VocalMap> {
    let m = clubcraft_analysis::vocals::VocalMap::from_bytes(&std::fs::read(vocal_map_path(hash)).ok()?)?;
    let g = a.grid?;
    (m.version == clubcraft_analysis::vocals::VERSION && (m.bpm - g.bpm).abs() < 1e-6 && (m.downbeat - g.downbeat).abs() < 1e-6).then_some(m)
}

/// The vocal map of a track ([`clubcraft_analysis::vocals`]), computed from the cached stems when
/// missing or stale (~1 s, mostly loading them; callers hold the heavy-work lock). `None`
/// without stems.
pub fn vocal_map(lib: &Library, id: TrackId) -> Result<Option<clubcraft_analysis::vocals::VocalMap>> {
    let t = lib.track(id)?.context("unknown track")?;
    let Some(a) = lib.analysis(id)? else { return Ok(None) };
    if let Some(m) = cached_vocal_map(&t.hash, &a) {
        return Ok(Some(m));
    }
    let Some(grid) = a.grid else { return Ok(None) };
    let sp = stems_path(&t.hash);
    if !sp.exists() {
        return Ok(None);
    }
    let st = clubcraft_stems::load_stem_track(&sp)?;
    let owned = OwnedStems::from_stem_track_partial(&st);
    drop(st);
    let m = clubcraft_analysis::vocals::compute(&owned.as_ref(), grid, a.duration);
    save_vocal_map(&t.hash, &m)?;
    Ok(Some(m))
}

// ------------------------------------------------------------------ functional labels

pub fn labels_path(hash: &str) -> std::path::PathBuf {
    clubcraft_core::paths::data_dir().join("Mashup").join(format!("{hash}.ccfl"))
}

fn save_labels(hash: &str, l: &clubcraft_analysis::labels::BarLabels) -> Result<()> {
    let p = labels_path(hash);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, l.to_bytes())?;
    Ok(())
}

/// Cached functional labels of a track if they are current (same grid as the analysis).
pub fn cached_labels(hash: &str, a: &clubcraft_core::Analysis) -> Option<clubcraft_analysis::labels::BarLabels> {
    let l = clubcraft_analysis::labels::BarLabels::from_bytes(&std::fs::read(labels_path(hash)).ok()?)?;
    let g = a.grid?;
    (l.version == clubcraft_analysis::labels::VERSION && (l.bpm - g.bpm).abs() < 1e-6 && (l.downbeat - g.downbeat).abs() < 1e-6).then_some(l)
}

/// The structure model's functional labels per bar (chorus, verse ...), computed from the cached
/// stems when missing or stale: ~20-30 s and ~2 GB while the model runs, so callers hold the
/// heavy-work lock. `None` without stems, without the model, or for tracks under 102 s.
pub fn labels(lib: &Library, id: TrackId) -> Result<Option<clubcraft_analysis::labels::BarLabels>> {
    let t = lib.track(id)?.context("unknown track")?;
    let Some(a) = lib.analysis(id)? else { return Ok(None) };
    if let Some(l) = cached_labels(&t.hash, &a) {
        return Ok(Some(l));
    }
    let Some(grid) = a.grid else { return Ok(None) };
    let sp = stems_path(&t.hash);
    if !sp.exists() {
        return Ok(None);
    }
    let st = clubcraft_stems::load_stem_track(&sp)?;
    let owned = OwnedStems::from_stem_track_partial(&st);
    drop(st);
    let Some(act) = clubcraft_analysis::allinone::shared_activations(&owned.as_ref()) else { return Ok(None) };
    let l = clubcraft_analysis::labels::BarLabels::from_activations(&act, grid, a.duration);
    save_labels(&t.hash, &l)?;
    Ok(Some(l))
}
