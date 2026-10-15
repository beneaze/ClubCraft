//! # clubcraft-stems — HTDemucs / Mel-Band RoFormer stem separation in Rust
//!
//! Splits a track into **drums, bass, other ("Melody"), vocals** ([`clubcraft_core::StemKind`]
//! order). [`Quality::Best`] takes the vocals from Kimberley Jensen's Mel-Band RoFormer
//! ([`roformer`]) and drums/bass from htdemucs_ft run on the instrumental; the other presets use
//! Meta's Hybrid Transformer Demucs v4 alone, reproducing `demucs.apply.apply_model` +
//! `demucs.separate` exactly (normalisation, 7.8 s segments with triangular cross-fades, shift
//! trick, bag-of-models weights). The STFT / iSTFT run in Rust ([`stft`]), the network runs on
//!
//! * **CoreML** (macOS, ML Program fp32 on CPU+GPU, `models/<name>.mlmodelc`) — ~0.3 s per segment
//!   on an M1 Pro, or
//! * **ONNX Runtime CPU** (`models/<name>.onnx`, ORT 1.20) — portable fallback, ~5x slower.
//!
//! Accuracy (Rust vs Python `demucs.apply_model` with identical input and shifts, real house
//! tracks): CoreML 110–128 dB SDR per stem for all presets, ORT 78–88 dB — i.e. identical for all
//! practical purposes.
//!
//! ## Progressive separation
//!
//! [`Separator::separate_into`] processes segments around the play position first and publishes
//! each [`clubcraft_core::STEM_CHUNK_FRAMES`] chunk as soon as every segment overlapping it is done
//! (with `Quality::Fast` the chunk under the playhead is ready after 2 model runs, ~0.7 s on an
//! M1 Pro; with `High` after 8 runs). Published data equals [`Separator::separate`] output
//! (bit-identical for `Fast`/`High`/`Best`; ≤1 ulp-level differences with several shifts).
//! With `Best` the vocal model runs lazily for the regions the Demucs windows need, so the
//! chunk under the playhead is ready after ~15 s (High: ~6 s; 5:17 track, M1 Pro).
//!
//! ```no_run
//! use clubcraft_stems::{Quality, Separator};
//! let audio = clubcraft_audio::decode_file("track.mp3".as_ref(), 44_100)?;
//! let sep = Separator::new(Quality::High)?;
//! let stems = sep.separate(&audio, &mut |p| { println!("{:.0}%", p * 100.0); true })?;
//! clubcraft_stems::save_stems_with_model(&clubcraft_core::library::stems_path("hash"), &stems, sep.model_name())?;
//! # anyhow::Ok(())
//! ```
//!
//! For live decks use [`Separator::separate_into`], which fills a shared [`StemTrack`] chunk by
//! chunk starting at the play position. Separated stems are cached as 8-channel 16-bit FLAC
//! (see [`cache`]).

pub mod backend;
pub mod cache;
pub mod model;
pub mod npy;
pub mod roformer;
mod pipeline;
pub mod stft;

pub use backend::BackendKind;
pub use roformer::Roformer;
pub use cache::{load_stem_track, load_stems, save_stem_track, save_stem_track_as, read_model_name, save_stems, save_stems_with_model};
pub use model::Manifest;

use anyhow::{bail, Result};
use backend::Backend;
use clubcraft_audio::AudioBuffer;
use clubcraft_core::{StemTrack, SAMPLE_RATE};
use pipeline::{Job, Plan};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Separation quality presets (cost relative to `Fast` in parentheses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Quality {
    /// `htdemucs`, one shifted pass, overlap 0.25 (1x). Good enough for live preview.
    Fast,
    /// `htdemucs_ft` (one fine-tuned model per stem), one shifted pass, overlap 0.25 (4x). Default.
    #[default]
    High,
    /// `htdemucs_ft`, 2 shifts, overlap 0.5 (~12x).
    Ultra,
    /// Best vocals: Mel-Band RoFormer (Kimberley Jensen, MIT) for vocals, `htdemucs_ft` drums and
    /// bass on the instrumental (`mix − vocals`), other = `instrumental − drums − bass`, so the
    /// four stems sum to the mix to float precision (~2.5x High: 205 s for a 5:17 track on an
    /// M1 Pro). Model name `melroformer+htdemucs_ft`.
    Best,
}

impl Quality {
    pub fn options(self) -> Options {
        match self {
            Quality::Fast => Options { model: "htdemucs".into(), shifts: 1, overlap: 0.25, ..Default::default() },
            Quality::High => Options { model: "htdemucs_ft".into(), shifts: 1, overlap: 0.25, ..Default::default() },
            Quality::Ultra => Options { model: "htdemucs_ft".into(), shifts: 2, overlap: 0.5, ..Default::default() },
            Quality::Best => Options {
                model: "htdemucs_ft".into(),
                shifts: 1,
                overlap: 0.25,
                vocals_model: Some("melroformer".into()),
                ..Default::default()
            },
        }
    }

    pub fn parse(s: &str) -> Option<Quality> {
        match s.to_ascii_lowercase().as_str() {
            "fast" => Some(Quality::Fast),
            "high" => Some(Quality::High),
            "ultra" => Some(Quality::Ultra),
            "best" => Some(Quality::Best),
            _ => None,
        }
    }
}

/// Full separation options (normally derived from a [`Quality`]).
#[derive(Debug, Clone)]
pub struct Options {
    /// Manifest name in the models directory: `htdemucs`, `htdemucs_ft` or `htdemucs_6s`
    /// (guitar and piano are folded into "other").
    pub model: String,
    /// Number of shifted passes, averaged (Demucs `--shifts`; 0 = no shift). Shifts are
    /// deterministic: pass `i` of `n` delays the input by `(2i + 1) / 2n * 0.5 s`.
    pub shifts: usize,
    /// Segment overlap (Demucs `--overlap`).
    pub overlap: f64,
    pub backend: BackendKind,
    /// Explicit shift amounts in samples (overrides `shifts`; for tests / reproducing Demucs runs).
    /// Demucs' random `offset` corresponds to a shift of `22050 - offset`.
    pub shift_samples: Option<Vec<usize>>,
    /// Dedicated vocal model (`melroformer`): vocals come from it, Demucs only provides drums and
    /// bass, and "other" is the remainder so the stems sum to the mix.
    pub vocals_model: Option<String>,
    /// Chunk overlap of the vocal model (`None` = the checkpoint's reference value, 2).
    pub vocals_overlap: Option<usize>,
    /// What Demucs separates when a vocal model is used.
    pub demucs_input: DemucsInput,
}

/// Input of the Demucs pass in [`Quality::Best`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DemucsInput {
    /// The instrumental `mix − vocals` (measured best).
    #[default]
    Instrumental,
    /// The original mix.
    Mix,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            model: "htdemucs_ft".into(),
            shifts: 1,
            overlap: 0.25,
            backend: BackendKind::Auto,
            shift_samples: None,
            vocals_model: None,
            vocals_overlap: None,
            demucs_input: DemucsInput::Instrumental,
        }
    }
}

impl Options {
    fn shift_list(&self, samplerate: u32) -> Vec<usize> {
        if let Some(s) = &self.shift_samples {
            return s.clone();
        }
        let max_shift = (0.5 * samplerate as f64) as usize;
        (0..self.shifts).map(|i| (2 * i + 1) * max_shift / (2 * self.shifts)).collect()
    }
}

/// Separated stems, planar stereo, in [`clubcraft_core::StemKind`] order
/// (drums, bass, other, vocals). Every channel has the length of the input.
#[derive(Debug, Clone, Default)]
pub struct Stems {
    pub sample_rate: u32,
    pub stems: [(Vec<f32>, Vec<f32>); 4],
}

impl Stems {
    pub fn frames(&self) -> usize {
        self.stems[0].0.len()
    }

    /// Copy into a complete [`StemTrack`].
    pub fn to_stem_track(&self, model: &str) -> StemTrack {
        StemTrack::from_planar(model, &self.stems)
    }

    /// Copy out of a (complete) [`StemTrack`]; missing chunks read as silence.
    pub fn from_stem_track(t: &StemTrack) -> Stems {
        let mut stems: [(Vec<f32>, Vec<f32>); 4] = Default::default();
        for (l, r) in stems.iter_mut() {
            l.reserve_exact(t.frames);
            r.reserve_exact(t.frames);
        }
        for c in 0..t.num_chunks() {
            let n = t.chunk_frames(c);
            match t.chunk(c) {
                Some(ch) => {
                    let data = ch.to_f32();
                    for f in data.as_chunks::<{ clubcraft_core::STEM_CHANNELS }>().0 {
                        for (s, (l, r)) in stems.iter_mut().enumerate() {
                            l.push(f[2 * s]);
                            r.push(f[2 * s + 1]);
                        }
                    }
                }
                None => {
                    for (l, r) in stems.iter_mut() {
                        l.resize(l.len() + n, 0.0);
                        r.resize(r.len() + n, 0.0);
                    }
                }
            }
        }
        Stems { sample_rate: SAMPLE_RATE, stems }
    }
}

/// Returned (inside `anyhow::Error`) when the caller cancels; test with `err.is::<Cancelled>()`.
#[derive(Debug, Clone, Copy)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stem separation cancelled")
    }
}
impl std::error::Error for Cancelled {}

/// A loaded separation model plus its inference backend. Loading takes a few seconds (CoreML
/// compiles GPU kernels on first use); keep one around and reuse it for many tracks. It is `Sync`,
/// but runs on one track at a time are most efficient.
pub struct Separator {
    man: Manifest,
    opts: Options,
    backend: Box<dyn Backend>,
    spectro: stft::Spectro,
    roformer: Option<roformer::Roformer>,
    name: String,
}

impl Separator {
    /// Load the preset's model from [`clubcraft_core::paths::models_dir`].
    pub fn new(quality: Quality) -> Result<Self> {
        Self::with_models_dir(quality, &clubcraft_core::paths::models_dir())
    }

    pub fn with_models_dir(quality: Quality, dir: &Path) -> Result<Self> {
        Self::with_options(dir, quality.options())
    }

    pub fn with_options(dir: &Path, opts: Options) -> Result<Self> {
        let man = Manifest::load(dir, &opts.model)?;
        let backend = backend::load(opts.backend, &man)?;
        let spectro = stft::Spectro::new(man.nfft, man.hop_length);
        let roformer = match &opts.vocals_model {
            Some(v) => {
                let mut rf = roformer::Roformer::load(dir, v, opts.backend)?;
                if let Some(o) = opts.vocals_overlap {
                    rf.man.num_overlap = o.max(1);
                }
                Some(rf)
            }
            None => None,
        };
        let name = match &roformer {
            Some(rf) => format!("{}+{}", rf.man.name, man.name),
            None => man.name.clone(),
        };
        log::info!("stem separator: {name} on {}", backend.name());
        Ok(Separator { man, opts, backend, spectro, roformer, name })
    }

    /// `"coreml-gpu"` or `"onnxruntime-cpu"`.
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Model name stored in [`StemTrack::model`] and the cache file: `htdemucs`, `htdemucs_ft`,
    /// `htdemucs_6s` or `melroformer+htdemucs_ft` ([`Quality::Best`]).
    pub fn model_name(&self) -> &str {
        &self.name
    }

    pub fn options(&self) -> &Options {
        &self.opts
    }

    pub fn manifest(&self) -> &Manifest {
        &self.man
    }

    /// Number of model invocations needed for `frames` samples (for time estimates).
    pub fn segments_for(&self, frames: usize) -> usize {
        let plan = Plan::new(frames, self.man.segment_samples, self.opts.overlap, &self.opts.shift_list(self.man.samplerate));
        let demucs_models = if self.roformer.is_some() { 2 } else { self.man.models.len() };
        plan.windows.len() * demucs_models + self.roformer.as_ref().map_or(0, |r| r.chunks_for(frames))
    }

    /// Separate a whole track. `progress(fraction)` is called after every model invocation;
    /// returning `false` cancels (the error is [`Cancelled`]). Input at any sample rate is
    /// resampled to 44.1 kHz and the stems are resampled back.
    pub fn separate(&self, audio: &AudioBuffer, progress: &mut dyn FnMut(f32) -> bool) -> Result<Stems> {
        if audio.sample_rate != self.man.samplerate {
            let res = clubcraft_audio::resample::resample(audio, self.man.samplerate);
            let mut st = self.separate(&res, progress)?;
            for (l, r) in st.stems.iter_mut() {
                for ch in [l, r] {
                    let mut v = clubcraft_audio::resample::resample_channel(ch, self.man.samplerate, audio.sample_rate);
                    v.resize(audio.frames(), 0.0);
                    *ch = v;
                }
            }
            st.sample_rate = audio.sample_rate;
            return Ok(st);
        }
        let stems = self.run(audio, None, 0, progress)?.unwrap_or_default();
        Ok(Stems { sample_rate: audio.sample_rate, stems })
    }

    /// Progressive separation for live decks: processes the segments around `start_frame` first,
    /// continues forward, then wraps to the start, and publishes every `STEM_CHUNK_FRAMES` chunk of
    /// `target` (via [`StemTrack::set_chunk`]) as soon as all segments overlapping it are done.
    /// Published chunks equal the result of [`Separator::separate`] (up to float summation order).
    /// With [`Quality::Best`] the vocal model is run on demand for the regions being processed.
    /// `target` must have `audio.frames()` frames; audio must be 44.1 kHz. Setting `cancel` stops
    /// early with a [`Cancelled`] error (already published chunks stay valid).
    pub fn separate_into(
        &self,
        audio: &AudioBuffer,
        target: &StemTrack,
        start_frame: usize,
        progress: &mut dyn FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<()> {
        if audio.sample_rate != self.man.samplerate {
            bail!("separate_into needs {} Hz audio (got {})", self.man.samplerate, audio.sample_rate);
        }
        if target.frames != audio.frames() {
            bail!("StemTrack has {} frames, audio has {}", target.frames, audio.frames());
        }
        let mut cb = |p: f32| {
            progress(p);
            !cancel.load(Ordering::Relaxed)
        };
        self.run(audio, Some(target), start_frame, &mut cb)?;
        Ok(())
    }

    /// Shared implementation: returns the planar stems when `target` is `None`.
    fn run(
        &self,
        audio: &AudioBuffer,
        target: Option<&StemTrack>,
        start_frame: usize,
        progress: &mut dyn FnMut(f32) -> bool,
    ) -> Result<Option<[(Vec<f32>, Vec<f32>); 4]>> {
        let len = audio.frames();
        if len == 0 {
            return Ok(Some(Default::default()));
        }
        let plan = Plan::new(len, self.man.segment_samples, self.opts.overlap, &self.opts.shift_list(self.man.samplerate));

        // Best: the vocal model runs lazily, chunk by chunk, as the Demucs windows (processed in
        // `plan.order`) need the instrumental; Demucs only does drums and bass.
        let pass = self.roformer.as_ref().map(|rf| roformer::VocalPass::new(rf, [&audio.left, &audio.right]));
        let wanted: Option<Vec<usize>> = pass.as_ref().map(|_| {
            (0..self.man.sources.len()).filter(|&s| matches!(self.man.sources[s].as_str(), "drums" | "bass")).collect()
        });
        // Demucs `separate.py` normalisation by the mix statistics. (For Best the instrumental is
        // normalised by the mix statistics too, so that it can be streamed; HTDemucs re-normalises
        // every segment internally, so this has no practical effect.)
        let (mean, std) = norm_stats(&audio.left, &audio.right);
        let cancel = AtomicBool::new(false);
        let x: [Vec<f32>; 2];
        let slice_src;
        let best_src;
        let source: &dyn pipeline::WindowSource = match &pass {
            None => {
                x = [&audio.left, &audio.right].map(|c| c.iter().map(|v| (v - mean) / std).collect::<Vec<f32>>());
                slice_src = pipeline::SliceSource([&x[0], &x[1]]);
                &slice_src
            }
            Some(p) => {
                best_src = BestSource {
                    pass: p,
                    mix: [&audio.left, &audio.right],
                    mean,
                    std,
                    instrumental: self.opts.demucs_input == DemucsInput::Instrumental,
                    cancel: &cancel,
                };
                &best_src
            }
        };
        let fin_fn = |start: usize, n: usize, planar: &mut [f32]| {
            if let Some(p) = &pass {
                let mut v = [vec![0f32; n], vec![0f32; n]];
                let [vl, vr] = &mut v;
                p.read(start, [vl, vr]);
                let mix = [&audio.left, &audio.right];
                for ch in 0..2 {
                    for i in 0..n {
                        let d = planar[ch * n + i];
                        let b = planar[(2 + ch) * n + i];
                        planar[(4 + ch) * n + i] = mix[ch][start + i] - v[ch][i] - d - b;
                        planar[(6 + ch) * n + i] = v[ch][i];
                    }
                }
            }
        };
        let finalize: Option<&pipeline::Finalize<'_>> = if pass.is_some() { Some(&fin_fn) } else { None };
        let job = Job {
            man: &self.man,
            backend: &*self.backend,
            spectro: &self.spectro,
            source,
            order: plan.order(start_frame),
            plan: &plan,
            scale: std,
            offset: mean,
            target,
            wanted: wanted.as_deref(),
            finalize,
        };
        // Progress in cost units: a RoFormer chunk costs ~5 Demucs segments (CoreML, M1 Pro).
        let n_rf = pass.as_ref().map_or(0, |p| p.plan.starts.len()) as f32;
        let n_dm = (plan.windows.len() * if pass.is_some() { 2 } else { self.man.models.len() }) as f32;
        let mut cb = |p: f32| {
            let rf_done = pass.as_ref().map_or(0, |p| p.chunks_done.load(Ordering::Relaxed)) as f32;
            let ok = progress((5.0 * rf_done + p * n_dm) / (5.0 * n_rf + n_dm).max(1.0));
            if !ok {
                cancel.store(true, Ordering::Relaxed);
            }
            ok
        };
        let acc = match job.run(&mut cb)? {
            Some(acc) => acc,
            None => return Err(Cancelled.into()),
        };
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        if target.is_some() {
            return Ok(None);
        }
        Ok(Some(acc.into_planar_with(std, mean, finalize)))
    }
}

/// Demucs input for [`Quality::Best`]: runs the vocal model on demand and serves the normalised
/// instrumental (or mix).
struct BestSource<'a> {
    pass: &'a roformer::VocalPass<'a>,
    mix: [&'a [f32]; 2],
    mean: f32,
    std: f32,
    instrumental: bool,
    cancel: &'a AtomicBool,
}

impl pipeline::WindowSource for BestSource<'_> {
    fn fill(&self, start: isize, l: usize, dst: &mut [f32]) -> Result<()> {
        let len = self.mix[0].len();
        let a = start.max(0) as usize;
        let b = ((start + l as isize).max(0) as usize).min(len);
        if a >= b {
            return Ok(());
        }
        for j in self.pass.plan.chunks_covering(a, b) {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            self.pass.ensure_chunks(j..j + 1)?;
        }
        let mut v = [vec![0f32; b - a], vec![0f32; b - a]];
        {
            let [vl, vr] = &mut v;
            self.pass.read(a, [vl, vr]);
        }
        for ch in 0..2 {
            let off = ch * l + (a as isize - start) as usize;
            for (k, t) in (a..b).enumerate() {
                let m = self.mix[ch][t] - if self.instrumental { v[ch][k] } else { 0.0 };
                dst[off + k] = (m - self.mean) / self.std;
            }
        }
        Ok(())
    }
}

/// Demucs `separate.py` normalisation: mean and (unbiased) std of the mono mix.
fn norm_stats(left: &[f32], right: &[f32]) -> (f32, f32) {
    let n = left.len();
    if n < 2 {
        return (0.0, 1.0);
    }
    let mono = |i: usize| (left[i] as f64 + right[i] as f64) * 0.5;
    let mean = (0..n).map(mono).sum::<f64>() / n as f64;
    let var = (0..n).map(|i| (mono(i) - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    let std = var.sqrt();
    (mean as f32, if std > 1e-8 { std as f32 } else { 1.0 })
}

#[allow(dead_code)]
fn assert_send_sync() {
    fn is<T: Send + Sync>() {}
    is::<Separator>();
}
