//! ClubCraft track analysis.
//!
//! Produces everything the studio and the live decks need from a track's audio, as a
//! [`clubcraft_core::Analysis`]: a constant-tempo beat grid with bar phase, musical key, EBU R128
//! loudness and a DJ gain, an energy rating, structural sections (intro / verse / build / drop /
//! breakdown / outro) and **automatically generated, beat-snapped hot cues** (start, mix-in,
//! vocal, build, drop, break, second drop / outro, mix-out). Stems (from `clubcraft-stems`) are
//! optional; when present they make beats, key and especially sections and cues more robust.
//!
//! # Entry points
//!
//! * [`analyze`] — full analysis (≈0.2–0.5 s for a 5-minute track in release builds, without
//!   decoding; with stems and the All-In-One model add ~15–30 s of model inference — pass
//!   `structure: StructureSource::Rules` to skip it, or [`analyze_with_activations`] with cached
//!   activations).
//! * [`analyze_with_stem_track`] — same, taking the stems as a [`StemTrack`].
//! * [`waveform()`] — multi-band (+ per-stem) waveform summary for the waveform views.
//! * Individually usable pieces: [`beatgrid::detect`], [`key::detect`],
//!   [`loudness::integrated_lufs`], [`structure::sections`] (+ [`structure::bar_features_for`]),
//!   [`cues::generate`], [`energy::rating`].
//!
//! All inputs are 44.1 kHz stereo (other rates work, stems must match the mix rate); positions
//! are seconds in the original track timeline.
//!
//! ```no_run
//! # use clubcraft_analysis::*;
//! let audio = clubcraft_audio::decode_file("track.mp3".as_ref(), 44_100).unwrap();
//! let a = analyze(&audio, None, &AnalyzeOptions::default());
//! println!("{:?} {:?} {} cues", a.grid, a.key.map(|k| k.camelot()), a.cues.len());
//! ```
//!
//! # Pipeline
//!
//! One shared front end ([`frontend`]: 1024-point / 256-hop STFT → 40 mel bands, 172 frames/s)
//! feeds the beat tracker, bar features and structure; key uses its own 16k-point STFT on the
//! harmonic signal; loudness runs on the raw stereo. Beat grid, key and loudness run in
//! parallel (rayon). See each module for the algorithm details:
//!
//! * [`beatgrid`]: SuperFlux onset envelope (drums stem when available) → autocorrelation comb
//!   tempo with a 124 BPM log-Gaussian prior → folded-phase segment regression (global constant
//!   grid fit, ~1 ms) → on-beat/off-beat decision from band-wise folded onsets → integer/half
//!   BPM snapping → bar phase = first beat with music.
//! * [`key`]: HPSS-style median filtering (or bass+other+vocals stems), tuning estimation,
//!   tuned chroma, EDM key profiles.
//! * [`structure`]: per-bar stem activities (real stems, or estimates from the mix), section
//!   boundaries from the All-In-One model ([`allinone`], with stems) or phrase-aware
//!   dynamic-programming segmentation (rules), activity-based labelling; adjacent sections
//!   never share a label.
//! * [`cues`]: 8 stable hot-cue slots + markers, all on downbeats; mix-in / mix-out on phrase
//!   lines counted from the grid anchor (the first downbeat with music).
//! * [`energy`], [`loudness`], [`mod@waveform`].
//!
//! # Evaluation
//!
//! `cargo run --release -p clubcraft-analysis --example eval` compares against DJ.Studio's and
//! Mixed In Key's analysis of the user's library; `--example analyze -- <file> [--stems <dir>]
//! [--bars]` prints one track's analysis.

#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

pub mod allinone;
pub mod beatgrid;
pub mod cocola;
pub mod cues;
mod dsp;
pub mod energy;
pub mod frontend;
pub mod key;
pub mod labels;
pub mod mashup;
pub mod loudness;
pub mod structure;
pub mod vocals;
pub mod waveform;

pub use beatgrid::BeatOptions;
pub use clubcraft_audio::AudioBuffer;
pub use waveform::waveform;

use clubcraft_core::{Analysis, StemKind, StemTrack, STEM_CHANNELS};
use frontend::Spectral;

/// Borrowed planar stems in [`StemKind`] order (drums, bass, other, vocals): `(left, right)` per
/// stem, same sample rate (44.1 kHz) and length as the mix (shorter stems are treated as silent
/// past their end).
#[derive(Clone, Copy, Debug)]
pub struct StemsRef<'a> {
    pub stems: [(&'a [f32], &'a [f32]); 4],
}

/// Alias used in the API docs: the four stems as a borrowed view.
pub type Stems4<'a> = StemsRef<'a>;

impl<'a> StemsRef<'a> {
    pub fn new(stems: [(&'a [f32], &'a [f32]); 4]) -> Self {
        StemsRef { stems }
    }

    pub fn get(&self, kind: StemKind) -> (&'a [f32], &'a [f32]) {
        self.stems[kind.index()]
    }

    pub fn frames(&self) -> usize {
        self.stems
            .iter()
            .map(|(l, r)| l.len().min(r.len()))
            .min()
            .unwrap_or(0)
    }

    /// Mono downmix of one stem.
    pub fn mono(&self, kind: StemKind) -> Vec<f32> {
        let (l, r) = self.get(kind);
        dsp::mono(l, r)
    }

    /// Weighted mono sum of the stems (`weights` in StemKind order).
    pub fn mix_mono(&self, weights: [f32; 4], len: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; len];
        for (s, w) in self.stems.iter().zip(weights) {
            if w == 0.0 {
                continue;
            }
            let (l, r) = s;
            let n = len.min(l.len()).min(r.len());
            let g = 0.5 * w;
            for i in 0..n {
                out[i] += g * (l[i] + r[i]);
            }
        }
        out
    }
}

/// Owned planar stems, e.g. collected from a [`StemTrack`].
#[derive(Clone, Debug, Default)]
pub struct OwnedStems {
    pub stems: [(Vec<f32>, Vec<f32>); 4],
}

impl OwnedStems {
    /// Collect a (complete) [`StemTrack`] into planar buffers. Returns `None` while the
    /// separation is still in progress (not every chunk is ready).
    pub fn from_stem_track(track: &StemTrack) -> Option<OwnedStems> {
        if !track.is_complete() {
            return None;
        }
        Some(Self::from_stem_track_partial(track))
    }

    /// Like [`OwnedStems::from_stem_track`] but accepts incomplete tracks (missing chunks are silent).
    pub fn from_stem_track_partial(track: &StemTrack) -> OwnedStems {
        let n = track.frames;
        let mut stems: [(Vec<f32>, Vec<f32>); 4] = Default::default();
        for s in stems.iter_mut() {
            *s = (vec![0.0; n], vec![0.0; n]);
        }
        for c in 0..track.num_chunks() {
            let Some(chunk) = track.chunk(c) else {
                continue;
            };
            let start = c * clubcraft_core::STEM_CHUNK_FRAMES;
            let data = chunk.to_f32();
            for (j, fr) in data.as_chunks::<STEM_CHANNELS>().0.iter().enumerate() {
                let f = start + j;
                if f >= n {
                    break;
                }
                for (s, st) in stems.iter_mut().enumerate() {
                    st.0[f] = fr[2 * s];
                    st.1[f] = fr[2 * s + 1];
                }
            }
        }
        OwnedStems { stems }
    }

    pub fn as_ref(&self) -> StemsRef<'_> {
        StemsRef {
            stems: [
                (&self.stems[0].0, &self.stems[0].1),
                (&self.stems[1].0, &self.stems[1].1),
                (&self.stems[2].0, &self.stems[2].1),
                (&self.stems[3].0, &self.stems[3].1),
            ],
        }
    }
}

/// Options for [`analyze`].
#[derive(Debug, Clone)]
pub struct AnalyzeOptions {
    /// Beat tracking options (tempo range / prior / snapping).
    pub beat: BeatOptions,
    /// Loudness target for `gain_db` in LUFS (integrated, EBU R128). Default **−10 LUFS**:
    /// close to the median of modern house/tech-house club masters (−7..−10 LUFS) so most
    /// tracks get a small negative gain, quiet older tracks are lifted, and there is ~10 dB of
    /// headroom budget left for summing two decks with a limiter.
    pub target_lufs: f32,
    /// Maximum absolute auto gain (dB).
    pub max_gain_db: f32,
    /// Name of the stems model (stored in `Analysis::stems_model` when stems are given).
    pub stems_model: Option<String>,
    /// Where section boundaries come from (see [`StructureSource`]).
    pub structure: StructureSource,
}

/// Source of the section boundaries / labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StructureSource {
    /// The All-In-One model when stems are given, the track is long enough (≥ 102.4 s) and
    /// `models/allin1.onnx` loads; the rule-based segmentation otherwise.
    #[default]
    Auto,
    /// Always the rule-based segmentation.
    Rules,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        AnalyzeOptions {
            beat: BeatOptions::default(),
            target_lufs: -10.0,
            max_gain_db: 12.0,
            stems_model: None,
            structure: StructureSource::Auto,
        }
    }
}

/// Shared intermediate representation computed once per analysis.
pub(crate) struct Prepared<'a> {
    pub audio: &'a AudioBuffer,
    pub sr: f64,
    pub mono: Vec<f32>,
    pub mix: Spectral,
    pub mix_log: Vec<f32>,
    /// Spectral front end of the drums stem.
    pub drums: Option<(Spectral, Vec<f32>)>,
    pub stems: Option<StemsRef<'a>>,
}

impl<'a> Prepared<'a> {
    pub fn new(audio: &'a AudioBuffer, stems: Option<StemsRef<'a>>) -> Prepared<'a> {
        let sr = audio.sample_rate as f64;
        let mono = audio.mono();
        let ((mix, mix_log), drums) = rayon::join(
            || {
                let s = Spectral::compute(&mono, sr);
                let l = s.log_mel();
                (s, l)
            },
            || {
                stems.map(|st| {
                    let d = st.mono(StemKind::Drums);
                    let s = Spectral::compute(&d, sr);
                    let l = s.log_mel();
                    (s, l)
                })
            },
        );
        Prepared {
            audio,
            sr,
            mono,
            mix,
            mix_log,
            drums,
            stems,
        }
    }

    pub fn duration(&self) -> f64 {
        self.audio.duration()
    }
}

/// Full analysis of a decoded track. `stems` (optional) must be the separated stems of the
/// same audio. The result has `version = Analysis::CURRENT_VERSION`; cues are
/// [`clubcraft_core::CueSource::Auto`].
pub fn analyze(audio: &AudioBuffer, stems: Option<&StemsRef>, opts: &AnalyzeOptions) -> Analysis {
    analyze_with_activations(audio, stems, opts, None)
}

/// Like [`analyze`], with precomputed All-In-One activations (e.g. cached). `None` runs the
/// model itself when [`AnalyzeOptions::structure`] and the inputs allow it.
pub fn analyze_with_activations(
    audio: &AudioBuffer,
    stems: Option<&StemsRef>,
    opts: &AnalyzeOptions,
    activations: Option<&allinone::Activations>,
) -> Analysis {
    let stems = stems.copied();
    let duration = audio.duration();
    let mut out = Analysis {
        version: Analysis::CURRENT_VERSION,
        duration,
        ..Default::default()
    };
    if audio.frames() < audio.sample_rate as usize {
        return out;
    }
    let t0 = std::time::Instant::now();
    let prep = Prepared::new(audio, stems);
    let t1 = t0.elapsed();
    let want_model = opts.structure == StructureSource::Auto && activations.is_none();
    let (((est, key), lufs), model_act) = rayon::join(
        || {
            rayon::join(
                || {
                    rayon::join(
                        || {
                            let t = std::time::Instant::now();
                            let r = beatgrid::estimate(&prep, &opts.beat);
                            log::debug!("beat grid {:.0?}", t.elapsed());
                            r
                        },
                        || {
                            let t = std::time::Instant::now();
                            let r = key::detect_prepared(&prep);
                            log::debug!("key {:.0?}", t.elapsed());
                            r
                        },
                    )
                },
                || loudness::integrated_lufs(&audio.left, &audio.right, audio.sample_rate),
            )
        },
        || match (want_model, stems.as_ref()) {
            (true, Some(st)) => allinone::shared_activations(st),
            _ => None,
        },
    );
    let act = if opts.structure == StructureSource::Rules {
        None
    } else {
        activations.or(model_act.as_ref())
    };
    let t2 = t0.elapsed();
    out.key = key;
    out.loudness_lufs = lufs;
    out.gain_db = lufs.map(|l| (opts.target_lufs - l).clamp(-opts.max_gain_db, opts.max_gain_db));
    if let Some(est) = est {
        let grid = est.grid;
        out.grid = Some(grid);
        let bars = structure::bar_features(&prep, &grid);
        let act = act.filter(|a| a.len() >= allinone::MIN_FRAMES);
        let seg = structure::segment_phrases(&bars, &grid, duration, act);
        out.sections = seg.sections;
        out.cues = cues::generate_from(&grid, &out.sections, &bars, &seg.phrases, duration);
        out.energy = Some(energy::rating_from(lufs, &bars, &out.sections, grid.bpm));
        log::debug!(
            "analysis: front end {t1:.0?}, grid/key/loudness {:.0?}, structure+cues {:.0?}",
            t2 - t1,
            t0.elapsed() - t2
        );
    } else {
        out.energy = lufs.map(energy::rating_simple);
    }
    if stems.is_some() {
        out.stems_model = opts.stems_model.clone();
    }
    out
}

/// Convenience: analyse with stems taken from a [`StemTrack`] (used only if complete).
pub fn analyze_with_stem_track(
    audio: &AudioBuffer,
    stems: Option<&StemTrack>,
    opts: &AnalyzeOptions,
) -> Analysis {
    let owned = stems.and_then(OwnedStems::from_stem_track);
    let mut o = opts.clone();
    if o.stems_model.is_none() {
        o.stems_model = stems.map(|s| s.model.clone());
    }
    match &owned {
        Some(s) => analyze(audio, Some(&s.as_ref()), &o),
        None => analyze(audio, None, &o),
    }
}
