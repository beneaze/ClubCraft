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
//!
//! Mashup compatibility is asymmetric (AutoMashup, 2025): the search always asks "vocals of A over
//! the instrumental of B".

use crate::dsp;
use crate::StemsRef;
use clubcraft_core::{BeatGrid, StemKind};

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
        let u32_ = |s: &[u8]| u32::from_le_bytes(s.try_into().unwrap());
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
            let v: Vec<f32> = raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
            let mut f = BarFeatures::default();
            f.vocal.copy_from_slice(&v[0..12]);
            f.bass.copy_from_slice(&v[12..24]);
            f.other.copy_from_slice(&v[24..36]);
            (f.vocal_db, f.bass_db, f.other_db, f.drums_db, f.other_vband_db) = (v[36], v[37], v[38], v[39], v[40]);
            bars.push(f);
        }
        Some(MashupFeatures { version, bpm, downbeat, beats_per_bar, first_bar, gain_db, section_bars, bars })
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
    MashupFeatures { version: FEATURE_VERSION, bpm: grid.bpm, downbeat: grid.downbeat, beats_per_bar: grid.beats_per_bar, first_bar, gain_db, section_bars, bars }
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
    /// Phrase length in bars.
    pub bars: usize,
    /// Allowed key shift of the vocals (± semitones).
    pub key_range: i32,
    /// Allowed tempo change of the vocals (fraction, e.g. 0.06 = ±6 %), half/double time allowed.
    pub tempo_tolerance: f64,
    /// Candidate instrumental start bars are multiples of this (phrase grid).
    pub step_bars: i32,
    /// Restrict the vocal region (start bar range, inclusive) and the instrumental start bars.
    pub vocal_bars: Option<(i32, i32)>,
    pub inst_bars: Option<(i32, i32)>,
    /// Matches kept per instrumental track.
    pub per_track: usize,
    pub max_results: usize,
}

impl Default for MatchOptions {
    fn default() -> Self {
        MatchOptions { bars: 16, key_range: 2, tempo_tolerance: 0.06, step_bars: 4, vocal_bars: None, inst_bars: None, per_track: 2, max_results: 30 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub vocal_track: i64,
    /// First bar of the vocal phrase in the vocal track.
    pub vocal_bar: i32,
    pub inst_track: i64,
    /// Bar of the instrumental where the vocal phrase starts.
    pub inst_bar: i32,
    pub bars: usize,
    /// Semitones applied to the vocals.
    pub key_shift: i32,
    /// Speed of the vocals relative to their original tempo (instrumental BPM / vocal BPM,
    /// folded to half / double time).
    pub tempo_ratio: f64,
    /// Overall 0..1.
    pub score: f32,
    pub harmony: f32,
    /// Weakest 4-bar stretch of the harmony.
    pub harmony_min: f32,
    pub masking: f32,
    pub residual_vocals: f32,
    /// Harmony per bar (for display; NaN where the vocals are silent).
    pub per_bar: Vec<f32>,
    /// Optional learned score (COCOLA re-ranking), 0..1.
    pub learned: Option<f32>,
}

/// Bars that carry vocals: weight 0..1 by level relative to the track's loudest vocals.
fn vocal_weights(f: &MashupFeatures, start: i32, n: usize, peak_db: f32) -> Vec<f32> {
    (0..n as i32).map(|i| f.bar(start + i).map(|b| ((b.vocal_db - (peak_db - 30.0)) / 20.0).clamp(0.0, 1.0)).unwrap_or(0.0)).collect()
}

fn peak(f: &MashupFeatures, sel: impl Fn(&BarFeatures) -> f32) -> f32 {
    let mut v: Vec<f32> = f.bars.iter().map(sel).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v.get(v.len().saturating_sub(1).min((v.len() as f32 * 0.95) as usize)).copied().unwrap_or(SILENT_DB)
}

/// The strongest vocal phrases of a track (start bars on the phrase grid), best first.
pub fn vocal_regions(f: &MashupFeatures, o: &MatchOptions, max: usize) -> Vec<i32> {
    let pk = peak(f, |b| b.vocal_db);
    let step = o.step_bars.max(1);
    let (lo, hi) = o.vocal_bars.unwrap_or((f.first_bar, f.last_bar() - o.bars as i32));
    let mut cands: Vec<(i32, f32)> = Vec::new();
    let mut b = lo.div_euclid(step) * step;
    if b < lo {
        b += step;
    }
    while b <= hi {
        let w = vocal_weights(f, b, o.bars, pk);
        let cover = w.iter().sum::<f32>() / o.bars as f32;
        // prefer phrases that start where the vocal starts and on section boundaries
        let starts = f.section_bars.contains(&b) as i32 as f32 * 0.05 + w.first().copied().unwrap_or(0.0) * 0.05;
        if cover > 0.35 {
            cands.push((b, cover + starts));
        }
        b += step;
    }
    cands.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out: Vec<i32> = Vec::new();
    for (b, _) in cands {
        // distinct phrases (no heavy overlap)
        if out.iter().all(|&o2| (o2 - b).abs() >= o.bars as i32 / 2) {
            out.push(b);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Tempo ratio folded to half / double time; `None` outside the tolerance.
pub fn tempo_ratio(vocal_bpm: f64, inst_bpm: f64, tol: f64) -> Option<f64> {
    let r = [1.0, 2.0, 0.5].iter().map(|m| inst_bpm / (vocal_bpm * m)).min_by(|a, b| (a.ln().abs()).total_cmp(&b.ln().abs()))?;
    ((r - 1.0).abs() <= tol + 1e-9).then_some(r)
}

/// Score the vocals of `v` (bars `vbar..vbar+bars`) over the instrumental of `i` from `ibar`.
pub fn score(v: &MashupFeatures, vbar: i32, i: &MashupFeatures, ibar: i32, key_shift: i32, o: &MatchOptions) -> Option<Match> {
    let ratio = tempo_ratio(v.bpm, i.bpm, o.tempo_tolerance)?;
    let vp = peak(v, |b| b.vocal_db);
    let ip = peak(i, |b| b.vocal_db);
    let w = vocal_weights(v, vbar, o.bars, vp);
    let kernels: Vec<Option<[f32; 12]>> = (0..o.bars as i32).map(|k| v.bar(vbar + k).map(|b| consonance_kernel(&rotate(&b.vocal, key_shift)))).collect();
    score_with(v, vbar, i, ibar, key_shift, ratio, &w, &kernels, ip, o)
}

#[allow(clippy::too_many_arguments)]
fn score_with(v: &MashupFeatures, vbar: i32, i: &MashupFeatures, ibar: i32, key_shift: i32, ratio: f64, w: &[f32], kernels: &[Option<[f32; 12]>], inst_vocal_peak: f32, o: &MatchOptions) -> Option<Match> {
    let n = o.bars;
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
        resid += w[k] * ((ib.vocal_db - (inst_vocal_peak - 24.0)) / 12.0).clamp(0.0, 1.0);
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
    let tempo_cost = (((ratio - 1.0).abs() / o.tempo_tolerance.max(0.01)) as f32).powi(2) * 0.08;
    let key_cost = key_shift.unsigned_abs() as f32 * 0.025;
    let phrase_bonus = if i.section_bars.contains(&ibar) { 0.03 } else { 0.0 } + if v.section_bars.contains(&vbar) { 0.02 } else { 0.0 };
    let score = (0.7 * harmony + 0.3 * hmin - 0.18 * masking - 0.12 * residual - tempo_cost - key_cost + phrase_bonus).clamp(0.0, 1.0);
    Some(Match {
        vocal_track: 0,
        vocal_bar: vbar,
        inst_track: 0,
        inst_bar: ibar,
        bars: n,
        key_shift,
        tempo_ratio: ratio,
        score,
        harmony,
        harmony_min: hmin,
        masking,
        residual_vocals: residual,
        per_bar,
        learned: None,
    })
}

/// Find the best places to put the vocals of `vocal` over the instrumentals of `candidates`.
/// `regions` are the vocal phrases to try (see [`vocal_regions`]).
pub fn find(vocal: (i64, &MashupFeatures), regions: &[i32], candidates: &[(i64, &MashupFeatures)], o: &MatchOptions) -> Vec<Match> {
    use rayon::prelude::*;
    let (vid, v) = vocal;
    let vp = peak(v, |b| b.vocal_db);
    // precomputed per region and key shift
    let prep: Vec<(i32, Vec<f32>, Vec<(i32, Vec<Option<[f32; 12]>>)>)> = regions
        .iter()
        .map(|&r| {
            let w = vocal_weights(v, r, o.bars, vp);
            let ks = (-o.key_range..=o.key_range)
                .map(|k| (k, (0..o.bars as i32).map(|j| v.bar(r + j).map(|b| consonance_kernel(&rotate(&b.vocal, k)))).collect()))
                .collect();
            (r, w, ks)
        })
        .collect();
    let mut all: Vec<Match> = candidates
        .par_iter()
        .filter(|(id, _)| *id != vid)
        .flat_map_iter(|(iid, f)| {
            let Some(ratio) = tempo_ratio(v.bpm, f.bpm, o.tempo_tolerance) else { return Vec::new() };
            let ip = peak(f, |b| b.vocal_db);
            let step = o.step_bars.max(1);
            let (lo, hi) = o.inst_bars.unwrap_or((f.first_bar, f.last_bar() - o.bars as i32));
            let mut found: Vec<Match> = Vec::new();
            let mut b = lo.div_euclid(step) * step;
            if b < lo {
                b += step;
            }
            while b <= hi.min(f.last_bar() - o.bars as i32) {
                for (r, w, ks) in &prep {
                    for (k, kern) in ks {
                        if let Some(mut m) = score_with(v, *r, f, b, *k, ratio, w, kern, ip, o) {
                            m.vocal_track = vid;
                            m.inst_track = *iid;
                            found.push(m);
                        }
                    }
                }
                b += step;
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
        MashupFeatures { version: FEATURE_VERSION, bpm, downbeat: 0.0, beats_per_bar: 4, first_bar: 0, gain_db: 0.0, section_bars: vec![0], bars }
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
        assert!(shifted.score > b.score + 0.3);
        let found = find((1, &v), &[0], &[(2, &good), (3, &bad)], &MatchOptions { bars: 4, key_range: 1, ..Default::default() });
        assert_eq!(found[0].inst_track, 2);
        assert!(found.iter().any(|m| m.inst_track == 3 && m.key_shift == 1));
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
