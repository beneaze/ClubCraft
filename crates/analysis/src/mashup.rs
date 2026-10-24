//! Mashup matching: which part of which track carries the vocals of another track.
//!
//! Follows the "mashability" idea of AutoMashUpper (Davies et al., 2014) and the vertical
//! mashability of Lee et al. (ISMIR 2015), computed on separated stems:
//!
//! * **Features** (per bar of the beat grid, from the stems): chroma of the vocal stem, of the
//!   bass and of the melody ("other") stem, and the levels of every stem plus the melody level in
//!   the vocal band (300 Hz - 3.4 kHz) that would mask a vocal.
//! * **Harmony**: for every bar, how consonant the (key-shifted) vocal notes are with the
//!   instrumental's notes: an interval-consonance weighting of the two chroma vectors (unison,
//!   fifths, thirds good; seconds, sevenths and the tritone bad), so a vocal that sits on chord
//!   tones scores high even when the chroma vectors differ.
//! * **Penalties**: masking by a loud melody in the vocal band, residual vocals of the
//!   instrumental track (its vocal stem is muted, but separation is not perfect), tempo change
//!   and key shift.
//! * **Placement**: where in the instrumental the vocals go. A drop or full groove in the body
//!   of the track, entered where it starts; intros, outros and breakdowns carry a vocal poorly
//!   even when nothing in them clashes with it.
//! * **Phrase**: which vocals: a hook that repeats in the track and starts where the singing
//!   starts, not in the middle of a line.
//!
//! Mashup compatibility is asymmetric (AutoMashup, 2025): the search always asks "vocals of A over
//! the instrumental of B".

use crate::dsp;
use crate::labels::{BarLabels, Label};
use crate::vocals::VocalMap;
use crate::StemsRef;
use clubcraft_core::{BeatGrid, Section, SectionKind, StemKind};

/// Bumped when the features change.
pub const FEATURE_VERSION: u32 = 1;

/// Features of one bar.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BarFeatures {
    /// L1-normalised, log-compressed chroma (C = 0).
    pub vocal: [f32; 12],
    pub bass: [f32; 12],
    pub other: [f32; 12],
    /// RMS levels (dBFS) of the stems.
    pub vocal_db: f32,
    pub bass_db: f32,
    pub other_db: f32,
    pub drums_db: f32,
    /// Level of the melody stem in the vocal band.
    pub other_vband_db: f32,
}

const FLOATS_PER_BAR: usize = 12 * 3 + 5;

/// Per-bar features of a track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MashupFeatures {
    pub version: u32,
    /// Grid the bars refer to.
    pub bpm: f64,
    pub downbeat: f64,
    pub beats_per_bar: u32,
    /// `bars[j]` is bar `first_bar + j` of the grid (bar 0 starts at the downbeat).
    pub first_bar: i32,
    /// Loudness correction the engine applies to the track (dB), so levels compare across tracks.
    pub gain_db: f32,
    /// Bars where an analysed section starts (phrase boundaries).
    pub section_bars: Vec<i32>,
    pub bars: Vec<BarFeatures>,
    /// The track's analysed sections (not stored in the feature file: they come from the
    /// analysis, see [`MashupFeatures::set_sections`]).
    pub sections: Vec<Section>,
    /// Where the track really sings, at 16th-note resolution (not stored in the feature file; see
    /// [`MashupFeatures::set_vocal_map`]). Needed only for the track that gives the vocals.
    pub vocal_map: Option<VocalMap>,
    /// The structure model's functional labels per bar (chorus, verse ...; not stored in the
    /// feature file, see [`MashupFeatures::set_labels`]). Needed only for the vocal track.
    pub labels: Option<BarLabels>,
}

const MAGIC: &[u8; 4] = b"CCMF";
const SILENT_DB: f32 = -90.0;

impl MashupFeatures {
    pub fn grid(&self) -> BeatGrid {
        BeatGrid { bpm: self.bpm, downbeat: self.downbeat, beats_per_bar: self.beats_per_bar }
    }

    pub fn bar(&self, bar: i32) -> Option<&BarFeatures> {
        usize::try_from(bar - self.first_bar).ok().and_then(|j| self.bars.get(j))
    }

    pub fn last_bar(&self) -> i32 {
        self.first_bar + self.bars.len() as i32
    }

    /// Time (s) where `bar` starts.
    pub fn bar_time(&self, bar: i32) -> f64 {
        self.grid().time_of_bar(bar as f64)
    }

    /// Take the sections (and the phrase boundaries) from the track's analysis, which may be
    /// newer than the feature file.
    pub fn set_sections(&mut self, sections: &[Section]) {
        self.sections = sections.to_vec();
        self.section_bars = sections.iter().map(|s| s.start_bar).collect();
        self.section_bars.dedup();
    }

    /// Attach the vocal map; ignored when it was made on another beat grid.
    pub fn set_vocal_map(&mut self, m: VocalMap) {
        if (m.bpm - self.bpm).abs() < 1e-6 && (m.downbeat - self.downbeat).abs() < 1e-6 && m.beats_per_bar == self.beats_per_bar {
            self.vocal_map = Some(m);
        }
    }

    /// Attach the functional labels; ignored when they were made on another beat grid.
    pub fn set_labels(&mut self, l: BarLabels) {
        if (l.bpm - self.bpm).abs() < 1e-6 && (l.downbeat - self.downbeat).abs() < 1e-6 && l.beats_per_bar == self.beats_per_bar {
            self.labels = Some(l);
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(64 + self.bars.len() * FLOATS_PER_BAR * 4);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&self.version.to_le_bytes());
        b.extend_from_slice(&self.bpm.to_le_bytes());
        b.extend_from_slice(&self.downbeat.to_le_bytes());
        b.extend_from_slice(&self.beats_per_bar.to_le_bytes());
        b.extend_from_slice(&self.first_bar.to_le_bytes());
        b.extend_from_slice(&self.gain_db.to_le_bytes());
        b.extend_from_slice(&(self.section_bars.len() as u32).to_le_bytes());
        for s in &self.section_bars {
            b.extend_from_slice(&s.to_le_bytes());
        }
        b.extend_from_slice(&(self.bars.len() as u32).to_le_bytes());
        for f in &self.bars {
            for v in f.vocal.iter().chain(&f.bass).chain(&f.other).chain(&[f.vocal_db, f.bass_db, f.other_db, f.drums_db, f.other_vband_db]) {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<MashupFeatures> {
        let mut p = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let s = b.get(p..p + n)?;
            p += n;
            Some(s)
        };
        if take(4)? != MAGIC {
            return None;
        }
        let u32_ = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap_or([0; 4]));
        let version = u32_(take(4)?);
        let bpm = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let downbeat = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let beats_per_bar = u32_(take(4)?);
        let first_bar = i32::from_le_bytes(take(4)?.try_into().ok()?);
        let gain_db = f32::from_le_bytes(take(4)?.try_into().ok()?);
        let ns = u32_(take(4)?) as usize;
        let mut section_bars = Vec::with_capacity(ns.min(4096));
        for _ in 0..ns {
            section_bars.push(i32::from_le_bytes(take(4)?.try_into().ok()?));
        }
        let n = u32_(take(4)?) as usize;
        let mut bars = Vec::with_capacity(n.min(100_000));
        for _ in 0..n {
            let raw = take(FLOATS_PER_BAR * 4)?;
            let v: Vec<f32> = raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
            let mut f = BarFeatures::default();
            f.vocal.copy_from_slice(&v[0..12]);
            f.bass.copy_from_slice(&v[12..24]);
            f.other.copy_from_slice(&v[24..36]);
            (f.vocal_db, f.bass_db, f.other_db, f.drums_db, f.other_vband_db) = (v[36], v[37], v[38], v[39], v[40]);
            bars.push(f);
        }
        Some(MashupFeatures { version, bpm, downbeat, beats_per_bar, first_bar, gain_db, section_bars, bars, sections: Vec::new(), vocal_map: None, labels: None })
    }
}

// ------------------------------------------------------------------ feature extraction

const SR: f64 = clubcraft_core::SAMPLE_RATE as f64;
const N_FFT: usize = 16384;
const HOP: usize = 4096;

/// Chroma per STFT frame within `fmin..fmax` (A440 tuning), plus the frame's share of spectral
/// power in `band` as (band power, total power).
fn chroma_frames(x: &[f32], fmin: f64, fmax: f64, band: Option<(f64, f64)>) -> Vec<([f32; 12], (f32, f32))> {
    let bin_hz = SR / N_FFT as f64;
    let mut map: Vec<(usize, usize, f32)> = Vec::new();
    for k in 1..N_FFT / 2 {
        let f = k as f64 * bin_hz;
        if f < fmin || f > fmax {
            continue;
        }
        let p = 69.0 + 12.0 * (f / 440.0).log2();
        let r = p.round();
        let w = (std::f64::consts::PI * (p - r)).cos().powi(2);
        if w < 0.05 {
            continue;
        }
        map.push((k, (r as i64).rem_euclid(12) as usize, w as f32));
    }
    let band = band.map(|(lo, hi)| ((lo / bin_hz) as usize, (hi / bin_hz) as usize));
    dsp::stft_map(x, N_FFT, HOP, |_, spec| {
        let mut c = [0f32; 12];
        for &(k, pc, w) in &map {
            c[pc] += spec[k].norm() * w;
        }
        let e = band.map(|(a, b)| (spec[a..b.min(spec.len())].iter().map(|v| v.norm_sqr()).sum::<f32>(), spec.iter().map(|v| v.norm_sqr()).sum::<f32>())).unwrap_or((0.0, 0.0));
        (c, e)
    })
}

fn normalize_chroma(c: &[f32; 12]) -> [f32; 12] {
    let mx = c.iter().cloned().fold(0.0f32, f32::max);
    let mut o = [0f32; 12];
    if mx <= 1e-9 {
        return o;
    }
    for i in 0..12 {
        o[i] = (1.0 + 10.0 * c[i] / mx).ln();
    }
    // keep only the salient notes: values below 40 % of the strongest are noise / overtones
    let top = o.iter().cloned().fold(0.0f32, f32::max);
    for v in &mut o {
        *v = (*v - 0.4 * top).max(0.0);
    }
    let s: f32 = o.iter().sum();
    if s > 0.0 {
        for v in &mut o {
            *v /= s;
        }
    }
    o
}

fn db(power: f64) -> f32 {
    if power <= 1e-12 {
        SILENT_DB
    } else {
        (10.0 * power.log10()) as f32
    }
}

/// Compute per-bar features from the separated stems of a track.
pub fn compute(stems: &StemsRef, grid: BeatGrid, duration: f64, gain_db: f32, section_starts: &[f64]) -> MashupFeatures {
    let frames = stems.frames();
    let first_bar = grid.bar_at(0.0).ceil() as i32;
    let last_bar = grid.bar_at(duration.min(frames as f64 / SR)).floor() as i32; // exclusive
    let n_bars = (last_bar - first_bar).max(0) as usize;
    let bar_of_time = |t: f64| -> Option<usize> {
        let b = grid.bar_at(t).floor() as i32 - first_bar;
        (b >= 0 && (b as usize) < n_bars).then_some(b as usize)
    };
    // Chroma per bar: sum of frame chroma (magnitude weighted), then normalised.
    let chroma_bars = |kind: StemKind, fmin: f64, fmax: f64, band: Option<(f64, f64)>| -> (Vec<[f32; 12]>, Vec<f64>) {
        let x = stems.mono(kind);
        let cf = chroma_frames(&x, fmin, fmax, band);
        let mut acc = vec![[0f32; 12]; n_bars];
        let mut be = vec![(0f64, 0f64); n_bars];
        for (i, (c, (eb, et))) in cf.iter().enumerate() {
            if let Some(b) = bar_of_time((i * HOP) as f64 / SR) {
                for k in 0..12 {
                    acc[b][k] += c[k];
                }
                be[b].0 += *eb as f64;
                be[b].1 += *et as f64;
            }
        }
        let band_share: Vec<f64> = be.iter().map(|(b, t)| if *t > 0.0 { b / t } else { 0.0 }).collect();
        (acc.iter().map(normalize_chroma).collect(), band_share)
    };
    let (vocal, _) = chroma_bars(StemKind::Vocals, 100.0, 2000.0, None);
    let (bass, _) = chroma_bars(StemKind::Bass, 30.0, 260.0, None);
    let (other, vband) = chroma_bars(StemKind::Other, 80.0, 4000.0, Some((300.0, 3400.0)));
    // RMS levels per bar in the time domain.
    let rms = |kind: StemKind| -> Vec<f32> {
        let (l, r) = stems.get(kind);
        (0..n_bars)
            .map(|j| {
                let t0 = grid.time_of_bar((first_bar + j as i32) as f64).max(0.0);
                let t1 = grid.time_of_bar((first_bar + j as i32 + 1) as f64);
                let (a, b) = ((t0 * SR) as usize, ((t1 * SR) as usize).min(l.len()).min(r.len()));
                if b <= a {
                    return SILENT_DB;
                }
                let s: f64 = (a..b).map(|i| 0.5 * (l[i] as f64 * l[i] as f64 + r[i] as f64 * r[i] as f64)).sum();
                db(s / (b - a) as f64)
            })
            .collect()
    };
    let (vdb, bdb, odb, ddb) = (rms(StemKind::Vocals), rms(StemKind::Bass), rms(StemKind::Other), rms(StemKind::Drums));
    let bars = (0..n_bars)
        .map(|j| BarFeatures {
            vocal: vocal[j],
            bass: bass[j],
            other: other[j],
            vocal_db: vdb[j],
            bass_db: bdb[j],
            other_db: odb[j],
            drums_db: ddb[j],
            // the melody stem's level restricted to the vocal band
            other_vband_db: if vband[j] > 0.0 { odb[j] + db(vband[j]) } else { SILENT_DB },
        })
        .collect();
    let mut section_bars: Vec<i32> = section_starts.iter().map(|&t| grid.bar_at(t).round() as i32).collect();
    section_bars.dedup();
    MashupFeatures { version: FEATURE_VERSION, bpm: grid.bpm, downbeat: grid.downbeat, beats_per_bar: grid.beats_per_bar, first_bar, gain_db, section_bars, bars, sections: Vec::new(), vocal_map: None, labels: None }
}

// ------------------------------------------------------------------ matching

/// Consonance of a vocal note `d` semitones above an instrumental note (`d` = 0..11). Directional:
/// a vocal a semitone above a chord tone (b9) clashes, a major seventh above it (maj7) is colour;
/// fifths, thirds and sixths are chord tones, ninths and sevenths mild extensions.
pub const CONSONANCE: [f32; 12] = [1.0, -1.0, 0.15, 0.5, 0.6, 0.25, -0.6, 0.8, 0.0, 0.45, 0.25, 0.0];

/// Rotate a chroma vector up by `k` semitones.
fn rotate(c: &[f32; 12], k: i32) -> [f32; 12] {
    let mut o = [0f32; 12];
    for (p, v) in c.iter().enumerate() {
        o[(p as i32 + k).rem_euclid(12) as usize] = *v;
    }
    o
}

/// `g[q] = Σ_p v[p] · C[p − q]`: dot `g` with an instrumental chroma gives the consonance.
fn consonance_kernel(v: &[f32; 12]) -> [f32; 12] {
    let mut g = [0f32; 12];
    for (q, gq) in g.iter_mut().enumerate() {
        for (p, vp) in v.iter().enumerate() {
            *gq += vp * CONSONANCE[(p + 12 - q) % 12];
        }
    }
    g
}

fn dot(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Harmonic content of an instrumental bar: bass (roots) and melody, each normalised.
fn instrumental_chroma(f: &BarFeatures) -> Option<[f32; 12]> {
    let (sb, so): (f32, f32) = (f.bass.iter().sum(), f.other.iter().sum());
    let (wb, wo) = (if sb > 0.0 && f.bass_db > -50.0 { 0.45 } else { 0.0 }, if so > 0.0 && f.other_db > -50.0 { 0.55 } else { 0.0 });
    if wb + wo == 0.0 {
        return None;
    }
    let mut u = [0f32; 12];
    for i in 0..12 {
        u[i] = (wb * f.bass[i] / sb.max(1e-9) + wo * f.other[i] / so.max(1e-9)) / (wb + wo);
    }
    Some(u)
}

/// Raw consonance of a vocal chroma (already key-shifted) over one instrumental bar with an
/// interval table; `bass_w` weights the bass against the melody stem. For experiments.
pub fn bar_consonance(v: &[f32; 12], inst: &BarFeatures, table: &[f32; 12], bass_w: f32) -> Option<f32> {
    let (sb, so): (f32, f32) = (inst.bass.iter().sum(), inst.other.iter().sum());
    let (wb, wo) = (if sb > 0.0 && inst.bass_db > -50.0 { bass_w } else { 0.0 }, if so > 0.0 && inst.other_db > -50.0 { 1.0 - bass_w } else { 0.0 });
    if wb + wo == 0.0 || v.iter().sum::<f32>() <= 0.0 {
        return None;
    }
    let mut c = 0.0;
    for q in 0..12 {
        let u = (wb * inst.bass[q] / sb.max(1e-9) + wo * inst.other[q] / so.max(1e-9)) / (wb + wo);
        for p in 0..12 {
            c += v[p] * u * table[(p + 12 - q) % 12];
        }
    }
    Some(c)
}

/// Raw consonance mapped to a 0..1 harmony score. Calibrated on separated house / pop tracks
/// (`examples/mashup_eval.rs`): random placements have a median of ~0.5 (p10 0.2, p90 0.83), a
/// vocal over its own instrumental ~0.65 (p90 0.9). With this table the original pairing beats
/// its ±1-2 semitone shifts 93 % of the time and random placements 78 % of the time
/// (chroma cosine similarity as in AutoMashUpper: 84 % / 72 %).
fn harmony01(c: f32) -> f32 {
    (5.4 * c - 0.718).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchOptions {
    /// Phrase length in bars (half as long is tried too when the full length would cut a line).
    pub bars: usize,
    /// Allowed key shift of the vocals (± semitones).
    pub key_range: i32,
    /// Allowed tempo change of the vocals (fraction, e.g. 0.06 = ±6 %), half/double time allowed.
    pub tempo_tolerance: f64,
    /// Phrase grid (bars) for tracks without sections.
    pub step_bars: i32,
    /// Restrict the vocal phrase starts and the instrumental start bars (inclusive ranges).
    pub vocal_bars: Option<(i32, i32)>,
    pub inst_bars: Option<(i32, i32)>,
    /// Matches kept per instrumental track.
    pub per_track: usize,
    pub max_results: usize,
    /// Which of the two tracks plays first in the mix.
    pub order: MashupOrder,
}

impl Default for MatchOptions {
    fn default() -> Self {
        MatchOptions { bars: 16, key_range: 2, tempo_tolerance: 0.06, step_bars: 4, vocal_bars: None, inst_bars: None, per_track: 2, max_results: 30, order: MashupOrder::VocalsFirst }
    }
}

/// How the two tracks of a mashup follow each other in a mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MashupOrder {
    /// The vocal track plays, then its vocals sing on over the next track's instrumental, which
    /// plays on (old vocals in the new song).
    #[default]
    VocalsFirst,
    /// The instrumental track plays, then the next track's vocals come in over it, and that
    /// track plays on (new vocals in the old song).
    InstrumentalFirst,
}

/// What a vocal phrase is in its song (from the structure model's labels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VocalRole {
    Chorus,
    Verse,
    Bridge,
    /// No labels, or another part.
    #[default]
    Other,
}

impl VocalRole {
    pub fn label(self) -> &'static str {
        match self {
            VocalRole::Chorus => "Chorus",
            VocalRole::Verse => "Verse",
            VocalRole::Bridge => "Bridge",
            VocalRole::Other => "Vocal",
        }
    }
}

/// A vocal phrase worth lifting into a mashup: whole bars of a sung section, starting and ending
/// with a line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VocalUnit {
    pub bar: i32,
    pub bars: usize,
    pub role: VocalRole,
    /// 0..1, see [`unit_quality`].
    pub quality: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub vocal_track: i64,
    /// First bar of the vocal phrase in the vocal track.
    pub vocal_bar: i32,
    pub vocal_role: VocalRole,
    pub inst_track: i64,
    /// Bar of the instrumental where the vocal phrase starts.
    pub inst_bar: i32,
    pub bars: usize,
    /// Semitones applied to the vocals.
    pub key_shift: i32,
    /// Speed of the vocals relative to their original tempo (instrumental BPM / vocal BPM,
    /// folded to half / double time).
    pub tempo_ratio: f64,
    /// Overall 0..1: the fit, scaled by the placement and the phrase, minus tempo and key cost.
    pub score: f32,
    /// How well the vocals sit on the instrumental (0..1): harmony, its weakest stretch, masking
    /// and leftover vocals (blended with the learned score after re-ranking).
    pub fit: f32,
    pub harmony: f32,
    /// Weakest 4-bar stretch of the harmony.
    pub harmony_min: f32,
    pub masking: f32,
    pub residual_vocals: f32,
    /// How well the spot in the instrumental carries this vocal (0..1): the right kind of
    /// section for the phrase (a chorus on the drop), entered where it starts, in the body of
    /// the track.
    pub placement: f32,
    /// How good the vocal phrase is (0..1, see [`unit_quality`]).
    pub phrase: f32,
    /// Tempo and key cost.
    pub cost: f32,
    /// Harmony per bar (for display; NaN where the vocals are silent).
    pub per_bar: Vec<f32>,
    /// Optional learned score (COCOLA re-ranking), 0..1.
    pub learned: Option<f32>,
    /// Which track plays first in a mix made from the match.
    pub order: MashupOrder,
}

impl Match {
    /// Blend a learned score (weight `w`) into the fit and update the overall score.
    pub fn set_learned(&mut self, learned: f32, w: f32) {
        self.learned = Some(learned);
        self.fit = (1.0 - w) * self.fit + w * learned;
        self.score = overall(self.fit, self.placement, self.phrase, self.cost);
    }
}

/// The placement and the phrase scale the fit rather than add to it: sparse intros have so few
/// notes that almost any vocal is "consonant" over them, and the best-fitting verse must still
/// lose to a good chorus over the drop.
fn overall(fit: f32, placement: f32, phrase: f32, cost: f32) -> f32 {
    (fit * (0.15 + 0.85 * placement) * (0.4 + 0.6 * phrase) - cost).clamp(0.0, 1.0)
}

/// Bars that carry vocals, weight 0..1: from the vocal map where there is one (real singing
/// only), else by level relative to the track's loudest vocals.
fn vocal_weights(f: &MashupFeatures, start: i32, n: usize, peak_db: f32) -> Vec<f32> {
    if let Some(m) = &f.vocal_map {
        return (0..n as i32).map(|i| if f.bar(start + i).is_some() { ((m.bar_sung(start + i) - 0.1) / 0.4).clamp(0.0, 1.0) } else { 0.0 }).collect();
    }
    (0..n as i32).map(|i| f.bar(start + i).map(|b| ((b.vocal_db - (peak_db - 30.0)) / 20.0).clamp(0.0, 1.0)).unwrap_or(0.0)).collect()
}

fn peak(f: &MashupFeatures, sel: impl Fn(&BarFeatures) -> f32) -> f32 {
    let mut v: Vec<f32> = f.bars.iter().map(sel).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(v.len().saturating_sub(1).min((v.len() as f32 * 0.95) as usize)).copied().unwrap_or(SILENT_DB)
}

fn section_at(f: &MashupFeatures, bar: i32) -> Option<&Section> {
    f.sections.iter().find(|s| bar >= s.start_bar && bar < s.start_bar + s.bars as i32)
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn cosine(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let (na, nb) = (dot(a, a).sqrt(), dot(b, b).sqrt());
    if na <= 1e-9 || nb <= 1e-9 {
        0.0
    } else {
        dot(a, b) / (na * nb)
    }
}

// ------------------------------------------------------------------ vocal units

/// How closely the vocal notes of the phrase come back elsewhere in the track (0..1), from the
/// per-bar chroma: the fallback for tracks without a vocal map.
fn repetition(f: &MashupFeatures, start: i32, n: usize, peak_db: f32) -> f32 {
    let w = vocal_weights(f, start, n, peak_db);
    let own: f32 = w.iter().sum();
    if own <= 0.0 {
        return 0.0;
    }
    let mut best = 0f32;
    // repeats sit on the same 4-bar grid as the phrase
    let mut c = f.first_bar + (start - f.first_bar).rem_euclid(4);
    while c + n as i32 <= f.last_bar() {
        if (c - start).abs() >= n as i32 {
            let wc = vocal_weights(f, c, n, peak_db);
            let mut s = 0f32;
            for k in 0..n {
                if let (Some(a), Some(b)) = (f.bar(start + k as i32), f.bar(c + k as i32)) {
                    s += w[k].min(wc[k]) * cosine(&a.vocal, &b.vocal);
                }
            }
            best = best.max(s / own);
        }
        c += 4;
    }
    // unrelated sung bars in the same key reach ~0.45, a repeated chorus ~0.85
    ((best - 0.45) / 0.4).clamp(0.0, 1.0)
}

/// Per-bar estimate without a vocal map: sung throughout, repeating, starting where the singing
/// starts (or at a section).
fn coarse_phrase(f: &MashupFeatures, start: i32, n: usize) -> f32 {
    let pk = peak(f, |b| b.vocal_db);
    let w = vocal_weights(f, start, n, pk);
    let cover = w.iter().sum::<f32>() / n.max(1) as f32;
    let before = vocal_weights(f, start - 1, 1, pk).first().copied().unwrap_or(0.0);
    let sung = w.iter().take(2).cloned().fold(0.0, f32::max);
    let at_section = if f.section_bars.contains(&start) { 0.7 } else { 0.0 };
    let entry = sung * (1.0 - 0.8 * before).max(at_section);
    smoothstep(cover / 0.6) * (0.4 + 0.3 * repetition(f, start, n, pk) + 0.3 * entry)
}

/// How much the phrase from bar `start` is the hook (0..1 prior): a chorus, or a verse that leads
/// into one (the second half chorus) — "the chorus and one verse" is what DJs lift.
fn chorus_prior(l: &BarLabels, start: i32, n: usize) -> f32 {
    let whole = l.mean(start, n, Label::Chorus);
    let second = l.mean(start + (n / 2) as i32, n - n / 2, Label::Chorus);
    // the chorus itself still comes first, by a little
    0.45 + 0.55 * smoothstep(whole / 0.7).max(0.95 * smoothstep(second / 0.7))
}

/// The role of the phrase from bar `start`: the label most of its bars carry.
fn role_at(f: &MashupFeatures, start: i32, n: usize) -> VocalRole {
    let Some(l) = &f.labels else { return VocalRole::Other };
    [(Label::Chorus, VocalRole::Chorus), (Label::Verse, VocalRole::Verse), (Label::Bridge, VocalRole::Bridge)]
        .into_iter()
        .map(|(lab, role)| (l.mean(start, n, lab), role))
        .filter(|(p, _)| *p > 0.35)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, r)| r)
        .unwrap_or(VocalRole::Other)
}

/// How good the vocal phrase of `n` bars from `start` is (0..1), the way a DJ picks an acapella:
///
/// * **The hook**: the chorus, or a verse running into it ([`chorus_prior`]), and a phrase that
///   comes back elsewhere in the track.
/// * **Clean boundaries**: it starts where a line starts (a pickup is fine) and ends in a break
///   ([`VocalMap::phrase`]); a phrase that would cut a line scores 0.
/// * **Real singing**: sung throughout, not a loop of chops, and not in bars the model calls
///   instrumental (vocal-stem energy there is an instrument in the vocal stem).
pub fn unit_quality(f: &MashupFeatures, start: i32, n: usize) -> f32 {
    // without labels nothing is known about the phrase: between a verse and a chorus
    let prior = f.labels.as_ref().map(|l| chorus_prior(l, start, n)).unwrap_or(0.8);
    let Some(m) = &f.vocal_map else { return prior * coarse_phrase(f, start, n) };
    let p = m.phrase(start, n);
    if p.start < 0.5 || p.end < 0.5 {
        return 0.0;
    }
    let instrumental = |bar: i32| f.labels.as_ref().map(|l| l.p(bar, Label::Inst) + l.p(bar, Label::Solo)).unwrap_or(0.0);
    let sung = (0..n as i32).map(|k| m.bar_sung(start + k) * (1.0 - instrumental(start + k)).max(0.0)).sum::<f32>() / n.max(1) as f32;
    // with labels the hook is mostly known; without, repetition is all there is
    let hook = if f.labels.is_some() { 0.6 + 0.4 * p.hook } else { 0.3 + 0.7 * p.hook };
    prior * hook * smoothstep(sung / 0.6) * (0.3 + 0.7 * p.lyrical) * (0.6 + 0.4 * p.start * p.end)
}

/// Quality of the phrase from `start` (see [`unit_quality`]); kept for experiments.
pub fn phrase_quality(f: &MashupFeatures, start: i32, n: usize) -> f32 {
    unit_quality(f, start, n)
}

/// The vocal phrases of a track worth lifting, best first: whole sung sections (chorus, verse,
/// bridge by the structure model; without labels the analysed sections, or the phrase grid),
/// each tried at its start and every 8 bars in, and every start of a sung passage after a
/// pause; at the full and at half length, with the start moved by up to two bars to where a
/// line starts.
pub fn vocal_units(f: &MashupFeatures, o: &MatchOptions, max: usize) -> Vec<VocalUnit> {
    let mut lens = vec![o.bars];
    if o.bars >= 16 {
        lens.push(o.bars / 2);
    }
    let mut starts: Vec<i32> = Vec::new();
    // where sections begin: a phrase that starts with its section (the top of the verse) beats
    // one that joins it later
    let mut heads: Vec<i32> = Vec::new();
    match &f.labels {
        Some(l) => {
            for r in l.runs() {
                if !matches!(r.label, Label::Chorus | Label::Verse | Label::Bridge) {
                    continue;
                }
                let end = r.start_bar + r.bars as i32;
                starts.extend((r.start_bar..end - 4).step_by(8));
                heads.push(r.start_bar);
            }
        }
        None if !f.sections.is_empty() => {
            for s in &f.sections {
                let end = s.start_bar + s.bars as i32;
                starts.extend((s.start_bar..end - 4).step_by(8));
                heads.push(s.start_bar);
            }
        }
        None => {
            let step = o.step_bars.max(1) as usize;
            starts.extend((f.first_bar.div_euclid(step as i32) * step as i32..f.last_bar()).step_by(step));
        }
    }
    // wherever a sung passage starts after a pause, labels or not
    if let Some(m) = &f.vocal_map {
        starts.extend((f.first_bar..f.last_bar()).filter(|&b| m.passage_starts(b)));
    }
    starts.sort_unstable();
    starts.dedup();
    let (lo, hi) = o.vocal_bars.unwrap_or((i32::MIN, i32::MAX));
    // the line search only means something with a vocal map
    let shifts: &[i32] = if f.vocal_map.is_some() { &[0, -1, 1, -2, 2] } else { &[0] };
    let mut cands: Vec<VocalUnit> = Vec::new();
    for &s0 in &starts {
        for &n in &lens {
            let best = shifts
                .iter()
                .map(|d| s0 + d)
                .filter(|&s| s >= f.first_bar && s + n as i32 <= f.last_bar() && (lo..=hi).contains(&s))
                .map(|s| {
                    let shorter = if n < o.bars { 0.85 } else { 1.0 };
                    let head = if heads.contains(&s) { 1.0 } else { 0.95 };
                    (s, unit_quality(f, s, n) * (1.0 - 0.04 * (s - s0).abs() as f32) * shorter * head)
                })
                .max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((s, q)) = best.filter(|(_, q)| *q > 0.05) {
                cands.push(VocalUnit { bar: s, bars: n, role: role_at(f, s, n), quality: q });
            }
        }
    }
    cands.sort_by(|a, b| b.quality.total_cmp(&a.quality));
    let mut out: Vec<VocalUnit> = Vec::new();
    for u in cands {
        // distinct phrases (no heavy overlap)
        if out.iter().all(|o2| (o2.bar - u.bar).abs() >= (o2.bars.min(u.bars) / 2) as i32) {
            out.push(u);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Start bars of [`vocal_units`]; kept for experiments.
pub fn vocal_regions(f: &MashupFeatures, o: &MatchOptions, max: usize) -> Vec<i32> {
    vocal_units(f, o, max).iter().map(|u| u.bar).collect()
}

/// A unit at a given bar (a phrase the user picked): its role and quality, no line search.
pub fn unit_at(f: &MashupFeatures, bar: i32, o: &MatchOptions) -> VocalUnit {
    VocalUnit { bar, bars: o.bars, role: role_at(f, bar, o.bars), quality: unit_quality(f, bar, o.bars).max(0.3) }
}

// ------------------------------------------------------------------ placement in the instrumental

/// How well a section of the instrumental suits a phrase of the given role. DJs put the chorus
/// on the drop and a verse or bridge on the breakdown or a groove; a build carries little.
fn role_fit(role: VocalRole, kind: Option<SectionKind>) -> f32 {
    use SectionKind::*;
    let Some(kind) = kind else { return 0.8 };
    match (role, kind) {
        (_, Intro | Outro) => 0.1,
        (VocalRole::Chorus, Drop) => 1.0,
        (VocalRole::Chorus, Verse) => 0.75,
        (VocalRole::Chorus, Breakdown) => 0.55,
        (VocalRole::Verse, Drop) => 0.8,
        (VocalRole::Verse, Verse | Breakdown) => 1.0,
        (VocalRole::Bridge, Breakdown) => 1.0,
        (VocalRole::Bridge, Verse) => 0.85,
        (VocalRole::Bridge, Drop) => 0.7,
        (VocalRole::Other, Drop) => 0.9,
        (VocalRole::Other, Verse) => 0.85,
        (VocalRole::Other, Breakdown) => 0.7,
        (_, Buildup) => 0.35,
    }
}

/// Per bar of `f`: how well it carries a vocal (0..1). Drops and grooves need drums and bass to
/// play (which holds even where the section labels are wrong); a breakdown carries a vocal
/// without them; intros and outros (the long drum loops of extended mixes) barely.
fn carry(f: &MashupFeatures) -> Vec<f32> {
    let (pd, pb) = (peak(f, |b| b.drums_db), peak(f, |b| b.bass_db));
    let on = |db: f32, pk: f32| ((db - (pk - 18.0)) / 12.0).clamp(0.0, 1.0);
    f.bars
        .iter()
        .enumerate()
        .map(|(j, b)| {
            let groove = on(b.drums_db, pd) * on(b.bass_db, pb);
            match section_at(f, f.first_bar + j as i32) {
                Some(s) => match s.kind {
                    SectionKind::Intro | SectionKind::Outro => 0.1,
                    SectionKind::Breakdown => 0.6 + 0.4 * s.energy,
                    SectionKind::Buildup => 0.5,
                    SectionKind::Drop | SectionKind::Verse => (0.7 + 0.3 * s.energy) * (0.3 + 0.7 * groove),
                },
                None => 0.3 + 0.7 * groove,
            }
        })
        .collect()
}

/// How well bars `ibar..ibar+n` of `f` carry a phrase of `role` (0..1): the bars themselves
/// ([`carry`]), the kind of section it enters ([`role_fit`]), entering where the section starts
/// or where the instrumental's own vocal comes in, room left by its own (muted) vocal, and where
/// it is in the track (the first bars are made for mixing in; when the instrumental comes second
/// it plays on to its end after the vocals, so a spot just before its outro leaves it nothing).
fn placement(f: &MashupFeatures, carry: &[f32], entries: &[i32], ibar: i32, n: usize, role: VocalRole, order: MashupOrder) -> f32 {
    let mean = (0..n as i32).map(|k| usize::try_from(ibar + k - f.first_bar).ok().and_then(|j| carry.get(j)).copied().unwrap_or(0.0)).sum::<f32>() / n.max(1) as f32;
    let section = section_at(f, ibar);
    let entry = match section {
        _ if entries.contains(&ibar) => 1.0,
        Some(s) if s.start_bar == ibar => 1.0,
        Some(s) if (ibar - s.start_bar) % 8 == 0 => 0.85,
        None if ibar.rem_euclid(8) == 0 => 0.85,
        _ => 0.6,
    };
    let len = f.bars.len().max(1) as f32;
    let (p0, p1) = ((ibar - f.first_bar) as f32 / len, (ibar - f.first_bar + n as i32) as f32 / len);
    // the instrumental plays on after the vocals only when it comes second
    let body = match order {
        MashupOrder::VocalsFirst => smoothstep(p0 / 0.18).min(smoothstep((0.95 - p1) / 0.2)),
        MashupOrder::InstrumentalFirst => smoothstep(p0 / 0.18).min(smoothstep((1.0 - p1) / 0.05)),
    };
    // where the instrumental sings itself, it has room for a vocal
    let pk = peak(f, |b| b.vocal_db);
    let room = vocal_weights(f, ibar, n, pk).iter().sum::<f32>() / n.max(1) as f32;
    (mean * entry * (0.4 + 0.6 * body) * role_fit(role, section.map(|s| s.kind)) * (0.75 + 0.25 * room)).clamp(0.0, 1.0)
}

/// Bars where the instrumental's own vocal comes in after a pause of two bars. Its vocal stem is
/// muted in the mashup, and its arrangement was made to carry a vocal there: putting the other
/// track's vocal in its place (a vocal swap) is the classic acapella mashup.
fn vocal_entries(f: &MashupFeatures) -> Vec<i32> {
    let pk = peak(f, |b| b.vocal_db);
    let w = vocal_weights(f, f.first_bar, f.bars.len(), pk);
    (2..w.len().saturating_sub(1)).filter(|&j| w[j - 1] < 0.25 && w[j - 2] < 0.25 && w[j].max(w[j + 1]) >= 0.5).map(|j| f.first_bar + j as i32).collect()
}

/// Where a phrase of `n` bars may go in `f`: section starts, the middle of sections at least twice
/// as long (the second half of a long drop) and where its own vocal comes in
/// ([`vocal_entries`]); never in an intro or outro. Without sections, the 8-bar lines.
fn slots(f: &MashupFeatures, o: &MatchOptions, n: usize) -> Vec<i32> {
    let mut out: Vec<i32> = Vec::new();
    if f.sections.is_empty() {
        out.extend((f.first_bar.div_euclid(8) * 8..f.last_bar()).step_by(8));
    } else {
        for s in &f.sections {
            if matches!(s.kind, SectionKind::Intro | SectionKind::Outro) {
                continue;
            }
            out.push(s.start_bar);
            if s.bars as usize >= 2 * n {
                out.push(s.start_bar + n as i32);
            }
        }
        out.extend(vocal_entries(f).into_iter().filter(|&b| !matches!(section_at(f, b).map(|s| s.kind), Some(SectionKind::Intro | SectionKind::Outro))));
        out.sort_unstable();
        out.dedup();
    }
    let (lo, hi) = o.inst_bars.unwrap_or((i32::MIN, i32::MAX));
    out.retain(|&b| b >= f.first_bar && b + n as i32 <= f.last_bar() && (lo..=hi).contains(&b));
    out
}

// ------------------------------------------------------------------ scoring

/// Tempo ratio folded to half / double time; `None` outside the tolerance.
pub fn tempo_ratio(vocal_bpm: f64, inst_bpm: f64, tol: f64) -> Option<f64> {
    let r = [1.0, 2.0, 0.5].iter().map(|m| inst_bpm / (vocal_bpm * m)).min_by(|a, b| (a.ln().abs()).total_cmp(&b.ln().abs()))?;
    ((r - 1.0).abs() <= tol + 1e-9).then_some(r)
}

/// A vocal unit prepared for scoring.
struct Phrase {
    unit: VocalUnit,
    weights: Vec<f32>,
    /// Consonance kernels per bar, for every key shift.
    kernels: Vec<(i32, Vec<Option<[f32; 12]>>)>,
}

impl Phrase {
    fn new(v: &MashupFeatures, unit: VocalUnit, keys: impl Iterator<Item = i32>) -> Phrase {
        let pk = peak(v, |b| b.vocal_db);
        let kernels = keys.map(|k| (k, (0..unit.bars as i32).map(|j| v.bar(unit.bar + j).map(|b| consonance_kernel(&rotate(&b.vocal, k)))).collect())).collect();
        Phrase { unit, weights: vocal_weights(v, unit.bar, unit.bars, pk), kernels }
    }
}

/// An instrumental prepared for scoring.
struct Inst<'a> {
    f: &'a MashupFeatures,
    carry: Vec<f32>,
    entries: Vec<i32>,
    vocal_peak: f32,
}

impl<'a> Inst<'a> {
    fn new(f: &'a MashupFeatures) -> Inst<'a> {
        Inst { f, carry: carry(f), entries: vocal_entries(f), vocal_peak: peak(f, |b| b.vocal_db) }
    }
}

/// Score the vocals of `v` (bars `vbar..vbar+bars`) over the instrumental of `i` from `ibar`.
pub fn score(v: &MashupFeatures, vbar: i32, i: &MashupFeatures, ibar: i32, key_shift: i32, o: &MatchOptions) -> Option<Match> {
    let ratio = tempo_ratio(v.bpm, i.bpm, o.tempo_tolerance)?;
    let p = Phrase::new(v, unit_at(v, vbar, o), std::iter::once(key_shift));
    let (k, kern) = p.kernels.first()?;
    score_with(v, &p, *k, kern, &Inst::new(i), ibar, ratio, o)
}

#[allow(clippy::too_many_arguments)]
fn score_with(v: &MashupFeatures, p: &Phrase, key_shift: i32, kernels: &[Option<[f32; 12]>], inst: &Inst, ibar: i32, ratio: f64, o: &MatchOptions) -> Option<Match> {
    let (i, w, vbar, n) = (inst.f, &p.weights, p.unit.bar, p.unit.bars);
    let mut per_bar = vec![f32::NAN; n];
    let (mut hs, mut ws, mut mask, mut resid) = (0f32, 0f32, 0f32, 0f32);
    for k in 0..n {
        let ib = i.bar(ibar + k as i32)?;
        let vb = v.bar(vbar + k as i32)?;
        if w[k] <= 0.0 {
            continue;
        }
        let Some(kern) = &kernels[k] else { continue };
        let h = match instrumental_chroma(ib) {
            Some(u) => harmony01(dot(kern, &u)),
            None => 0.5, // only drums / silence under the vocal: neutral
        };
        per_bar[k] = h;
        hs += w[k] * h;
        ws += w[k];
        // masking: melody louder than the vocal in the vocal band (after loudness correction)
        let m = ((ib.other_vband_db + i.gain_db) - (vb.vocal_db + v.gain_db) + 3.0) / 12.0;
        mask += w[k] * m.clamp(0.0, 1.0);
        // residual vocals of the instrumental track
        resid += w[k] * ((ib.vocal_db - (inst.vocal_peak - 24.0)) / 12.0).clamp(0.0, 1.0);
    }
    if ws < n as f32 * 0.25 {
        return None; // phrase has too little vocal
    }
    let harmony = hs / ws;
    // weakest 4-bar window
    let mut hmin = harmony;
    for s in (0..n.saturating_sub(3)).step_by(2) {
        let (mut a, mut b) = (0f32, 0f32);
        for k in s..(s + 4).min(n) {
            if !per_bar[k].is_nan() {
                a += w[k] * per_bar[k];
                b += w[k];
            }
        }
        if b > 0.5 {
            hmin = hmin.min(a / b);
        }
    }
    let (masking, residual) = (mask / ws, resid / ws);
    // the instrumental's vocal stem is muted in the mashup; what is left of its vocals is
    // separation bleed in the other stems, a fraction of this level
    let fit = (0.7 * harmony + 0.3 * hmin - 0.18 * masking - 0.04 * residual).clamp(0.0, 1.0);
    let placement = placement(i, &inst.carry, &inst.entries, ibar, n, p.unit.role, o.order);
    // when the vocal track comes second it plays on after its phrase: a phrase just before its
    // outro leaves it nothing
    let tail = match o.order {
        MashupOrder::VocalsFirst => 1.0,
        MashupOrder::InstrumentalFirst => 0.5 + 0.5 * smoothstep((0.95 - (vbar + n as i32 - v.first_bar) as f32 / v.bars.len().max(1) as f32) / 0.2),
    };
    let tempo_cost = (((ratio - 1.0).abs() / o.tempo_tolerance.max(0.01)) as f32).powi(2) * 0.08;
    // listeners prefer vocals in their own key (HKUST); DJs shift one or two semitones at most
    let cost = tempo_cost + key_shift.unsigned_abs() as f32 * 0.06;
    Some(Match {
        vocal_track: 0,
        vocal_bar: vbar,
        vocal_role: p.unit.role,
        inst_track: 0,
        inst_bar: ibar,
        bars: n,
        key_shift,
        tempo_ratio: ratio,
        score: overall(fit, placement, p.unit.quality * tail, cost),
        fit,
        harmony,
        harmony_min: hmin,
        masking,
        residual_vocals: residual,
        placement,
        phrase: p.unit.quality * tail,
        cost,
        per_bar,
        learned: None,
        order: o.order,
    })
}

/// Find the best places to put the vocals of `vocal` over the instrumentals of `candidates`:
/// every unit (see [`vocal_units`]) at every slot of every instrumental (see [`slots`]) and key
/// shift.
pub fn find(vocal: (i64, &MashupFeatures), units: &[VocalUnit], candidates: &[(i64, &MashupFeatures)], o: &MatchOptions) -> Vec<Match> {
    use rayon::prelude::*;
    let (vid, v) = vocal;
    let phrases: Vec<Phrase> = units.iter().map(|&u| Phrase::new(v, u, -o.key_range..=o.key_range)).collect();
    let mut all: Vec<Match> = candidates
        .par_iter()
        .filter(|(id, _)| *id != vid)
        .flat_map_iter(|(iid, f)| {
            let Some(ratio) = tempo_ratio(v.bpm, f.bpm, o.tempo_tolerance) else { return Vec::new() };
            let inst = Inst::new(f);
            let mut found: Vec<Match> = Vec::new();
            for p in &phrases {
                for b in slots(f, o, p.unit.bars) {
                    for (k, kern) in &p.kernels {
                        if let Some(mut m) = score_with(v, p, *k, kern, &inst, b, ratio, o) {
                            m.vocal_track = vid;
                            m.inst_track = *iid;
                            found.push(m);
                        }
                    }
                }
            }
            found.sort_by(|a, b| b.score.total_cmp(&a.score));
            // best few distinct placements per track
            let mut keep: Vec<Match> = Vec::new();
            for m in found {
                if keep.iter().all(|k| (k.inst_bar - m.inst_bar).abs() >= o.bars as i32 / 2 || k.vocal_bar != m.vocal_bar) {
                    keep.push(m);
                }
                if keep.len() >= o.per_track {
                    break;
                }
            }
            keep
        })
        .collect();
    all.sort_by(|a, b| b.score.total_cmp(&a.score));
    all.truncate(o.max_results);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(vocal: &[usize], bass: &[usize], other: &[usize]) -> BarFeatures {
        let c = |notes: &[usize]| {
            let mut c = [0f32; 12];
            for &n in notes {
                c[n] = 1.0 / notes.len() as f32;
            }
            c
        };
        BarFeatures { vocal: c(vocal), bass: c(bass), other: c(other), vocal_db: -20.0, bass_db: -20.0, other_db: -24.0, drums_db: -18.0, other_vband_db: -40.0 }
    }

    fn track(bars: Vec<BarFeatures>, bpm: f64) -> MashupFeatures {
        MashupFeatures { version: FEATURE_VERSION, bpm, downbeat: 0.0, beats_per_bar: 4, first_bar: 0, gain_db: 0.0, section_bars: vec![0], bars, sections: Vec::new(), vocal_map: None, labels: None }
    }

    #[test]
    fn consonant_beats_dissonant() {
        // vocal sings A-C-E (A minor); instrumental plays A minor vs. A# major
        let o = MatchOptions { bars: 4, key_range: 0, ..Default::default() };
        let v = track(vec![bar(&[9, 0, 4], &[], &[]); 4], 124.0);
        let good = track(vec![bar(&[], &[9], &[9, 0, 4]); 4], 124.0);
        let bad = track(vec![bar(&[], &[10], &[10, 2, 5]); 4], 124.0);
        let g = score(&v, 0, &good, 0, 0, &o).unwrap();
        let b = score(&v, 0, &bad, 0, 0, &o).unwrap();
        assert!(g.harmony > 0.8 && b.harmony < 0.45, "good {} bad {}", g.harmony, b.harmony);
        // a one-semitone shift fixes the bad pairing
        let shifted = score(&v, 0, &bad, 0, 1, &o).unwrap();
        assert!(shifted.fit > b.fit + 0.3);
        let unit = VocalUnit { bar: 0, bars: 4, role: VocalRole::Other, quality: 1.0 };
        let found = find((1, &v), &[unit], &[(2, &good), (3, &bad)], &MatchOptions { bars: 4, key_range: 1, ..Default::default() });
        assert_eq!(found[0].inst_track, 2);
        assert!(found.iter().any(|m| m.inst_track == 3 && m.key_shift == 1));
    }

    fn section(kind: SectionKind, start_bar: i32, bars: u32) -> Section {
        Section { kind, start: start_bar as f64 * 2.0, end: (start_bar + bars as i32) as f64 * 2.0, start_bar, bars, energy: 1.0, stems: [1.0; 4] }
    }

    #[test]
    fn vocals_go_over_the_drop_not_the_intro() {
        // the intro (drums and one bass note) has nothing to clash with and fits the A-minor
        // vocal at least as well as the drop's chords; the drop must still win
        let o = MatchOptions { bars: 16, key_range: 0, ..Default::default() };
        let v = track(vec![bar(&[9, 0, 4], &[], &[]); 64], 124.0);
        let mut intro = bar(&[], &[9], &[]);
        intro.bass_db = -40.0;
        let mut bars = vec![intro; 32];
        bars.extend(vec![bar(&[], &[9], &[9, 0, 4, 7]); 32]);
        bars.extend(vec![intro; 32]);
        let mut inst = track(bars, 124.0);
        inst.set_sections(&[section(SectionKind::Intro, 0, 32), section(SectionKind::Drop, 32, 32), section(SectionKind::Outro, 64, 32)]);
        let at = |b: i32| score(&v, 16, &inst, b, 0, &o).unwrap();
        assert!(at(0).harmony >= at(32).harmony - 0.05, "intro {} drop {}", at(0).harmony, at(32).harmony);
        assert!(at(32).placement > 2.0 * at(0).placement);
        let unit = VocalUnit { bar: 16, bars: 16, role: VocalRole::Chorus, quality: 1.0 };
        let found = find((1, &v), &[unit], &[(2, &inst)], &MatchOptions { per_track: 3, ..o });
        assert_eq!(found[0].inst_bar, 32, "{found:?}");
        // entering with the drop beats entering halfway through it
        assert!(at(32).placement > at(40).placement);
    }

    #[test]
    fn phrases_start_where_the_singing_starts() {
        let mut silent = bar(&[], &[], &[]);
        silent.vocal_db = SILENT_DB;
        let mut bars = vec![silent; 8];
        bars.extend(vec![bar(&[9, 0, 4], &[], &[]); 40]);
        let v = track(bars, 124.0);
        let o = MatchOptions { bars: 16, ..Default::default() };
        assert!(phrase_quality(&v, 8, 16) > phrase_quality(&v, 12, 16) + 0.1);
        assert_eq!(vocal_regions(&v, &o, 1), vec![8]);
    }

    fn labelled(seq: &[(Label, usize)]) -> BarLabels {
        let mut probs = Vec::new();
        for &(l, n) in seq {
            for _ in 0..n {
                let mut p = [0.02f32; 10];
                p[l.index()] = 0.8;
                probs.push(p);
            }
        }
        BarLabels { version: crate::labels::VERSION, bpm: 124.0, downbeat: 0.0, beats_per_bar: 4, first_bar: 0, probs }
    }

    #[test]
    fn the_chorus_is_the_phrase_and_goes_on_the_drop() {
        let o = MatchOptions { bars: 16, key_range: 0, ..Default::default() };
        let mut v = track(vec![bar(&[9, 0, 4], &[], &[]); 64], 124.0);
        v.set_labels(labelled(&[(Label::Verse, 16), (Label::Chorus, 16), (Label::Verse, 16), (Label::Chorus, 16)]));
        let units = vocal_units(&v, &o, 4);
        assert_eq!((units[0].role, units[0].bar % 32), (VocalRole::Chorus, 16), "{units:?}");
        // the same instrumental bars throughout: only the sections differ
        let mut inst = track(vec![bar(&[], &[9], &[9, 0, 4]); 80], 124.0);
        inst.set_sections(&[section(SectionKind::Intro, 0, 16), section(SectionKind::Breakdown, 16, 16), section(SectionKind::Drop, 32, 32), section(SectionKind::Outro, 64, 16)]);
        let best = |role: VocalRole| {
            let u = VocalUnit { bar: 16, bars: 16, role, quality: 1.0 };
            find((1, &v), &[u], &[(2, &inst)], &o)[0].inst_bar
        };
        assert_eq!(best(VocalRole::Chorus), 32);
        assert_eq!(best(VocalRole::Verse), 16);
        // never the intro or the outro
        assert!(slots(&inst, &o, 16).iter().all(|&b| (16..64).contains(&b)));
    }

    #[test]
    fn the_vocals_can_take_over_where_the_instrumental_sings() {
        // a breakdown whose own vocal pauses for two bars and comes back at bar 30, mid-section
        let mut inst_bars = vec![bar(&[], &[9], &[9, 0, 4]); 64];
        for (j, b) in inst_bars.iter_mut().enumerate() {
            b.vocal_db = if (28..30).contains(&j) || j < 16 { SILENT_DB } else { -20.0 };
        }
        let mut inst = track(inst_bars, 124.0);
        inst.set_sections(&[section(SectionKind::Intro, 0, 16), section(SectionKind::Breakdown, 16, 24), section(SectionKind::Drop, 40, 24)]);
        let o = MatchOptions { bars: 16, key_range: 0, ..Default::default() };
        assert!(slots(&inst, &o, 16).contains(&30), "{:?}", slots(&inst, &o, 16));
        let v = track(vec![bar(&[9, 0, 4], &[], &[]); 32], 124.0);
        let swap = score(&v, 0, &inst, 30, 0, &o).unwrap();
        let off_line = score(&v, 0, &inst, 26, 0, &o).unwrap();
        assert!(swap.placement > off_line.placement, "{} {}", swap.placement, off_line.placement);
        // its own (muted) vocal costs the fit next to nothing
        assert!(swap.fit > 0.75, "{swap:?}");
    }

    #[test]
    fn tempo_folding_and_limits() {
        assert!((tempo_ratio(124.0, 126.0, 0.06).unwrap() - 126.0 / 124.0).abs() < 1e-9);
        assert!((tempo_ratio(64.0, 126.0, 0.06).unwrap() - 126.0 / 128.0).abs() < 1e-9);
        assert!(tempo_ratio(110.0, 128.0, 0.06).is_none());
    }

    #[test]
    fn bytes_roundtrip() {
        let f = track(vec![bar(&[1], &[2], &[3]); 3], 122.5);
        assert_eq!(MashupFeatures::from_bytes(&f.to_bytes()).unwrap(), f);
    }
}
