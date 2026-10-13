//! Musical key detection.
//!
//! * **Harmonic signal**: with stems `bass + other + 0.5 * vocals` (drums removed); without
//!   stems the mix spectrogram is median filtered along time (HPSS-style harmonic part).
//! * **Spectrum**: 16384-point STFT (2.7 Hz resolution, needed for the bass octave), hop 4096.
//! * **Tuning**: magnitude-weighted circular mean of spectral peak deviations from the
//!   equal-tempered A440 grid.
//! * **Pitch salience / chroma**: bins are mapped to tuning-corrected semitones (41 Hz .. 2 kHz)
//!   with a raised-cosine weight, folded to 12 pitch classes; bass (< 130 Hz) and treble
//!   chroma are accumulated separately. Each frame is log-compressed and L1-normalised, frames
//!   are weighted by their harmonic energy. The bass chroma is weighted 0 by default: in
//!   the reference evaluation it hurt (kick drums and sub-bass slides smear it).
//! * **Profiles**: correlation against 24 rotations of key profiles suited to electronic dance
//!   music (by default a blend of Sha'ath's KeyFinder and Faraldo's EDMA profiles, see
//!   [`Profile`]).

use crate::dsp;
use crate::StemsRef;
use clubcraft_core::Key;

/// Key templates (12 values, starting at the tonic) for major and minor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Default: mean of the z-normalised Sha'ath and EDMA profiles (best agreement with Mixed
    /// In Key / DJ.Studio keys on the reference set: 75 % exact, 87 % exact-or-relative).
    EdmBlend,
    /// Faraldo et al. 2016, "edma" (derived from electronic dance music).
    Edma,
    /// Temperley 1999 (Kostka-Payne).
    Temperley,
    /// Ibrahim Sha'ath's KeyFinder profiles.
    Shaath,
    /// Krumhansl-Kessler probe tone profiles.
    Krumhansl,
}

impl Profile {
    pub fn templates(self) -> ([f32; 12], [f32; 12]) {
        match self {
            Profile::EdmBlend => {
                let z = |v: [f32; 12]| {
                    let m = v.iter().sum::<f32>() / 12.0;
                    let sd = (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / 12.0).sqrt();
                    v.map(|x| (x - m) / sd)
                };
                let (a, b) = Profile::Shaath.templates();
                let (c, d) = Profile::Edma.templates();
                let (a, b, c, d) = (z(a), z(b), z(c), z(d));
                (std::array::from_fn(|i| 0.5 * (a[i] + c[i])), std::array::from_fn(|i| 0.5 * (b[i] + d[i])))
            }
            Profile::Edma => (
                [0.1652, 0.0475, 0.0829, 0.0669, 0.0999, 0.0927, 0.0529, 0.1316, 0.0522, 0.0744, 0.0694, 0.0643],
                [0.1724, 0.0400, 0.0761, 0.1205, 0.0504, 0.0886, 0.0415, 0.1353, 0.0730, 0.0513, 0.0916, 0.0509],
            ),
            Profile::Temperley => (
                [5.0, 2.0, 3.5, 2.0, 4.5, 4.0, 2.0, 4.5, 2.0, 3.5, 1.5, 4.0],
                [5.0, 2.0, 3.5, 4.5, 2.0, 4.0, 2.0, 4.5, 3.5, 2.0, 1.5, 4.0],
            ),
            Profile::Shaath => (
                [7.24, 3.50, 3.58, 2.85, 5.82, 4.56, 2.45, 6.99, 3.39, 4.56, 4.07, 4.46],
                [7.00, 3.14, 4.36, 5.40, 3.67, 4.09, 3.91, 6.20, 3.63, 3.15, 3.81, 2.69],
            ),
            Profile::Krumhansl => (
                [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88],
                [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17],
            ),
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyOptions {
    pub profile: Profile,
    /// Weight of the bass chroma relative to the treble chroma.
    pub bass_weight: f32,
}

impl Default for KeyOptions {
    fn default() -> Self {
        KeyOptions { profile: Profile::EdmBlend, bass_weight: 0.0 }
    }
}

/// Detailed key result.
#[derive(Debug, Clone)]
pub struct KeyEstimate {
    pub key: Key,
    /// Correlation of the best key (−1..1) and margin to the runner-up.
    pub score: f32,
    pub margin: f32,
    /// Tuning offset in cents relative to A440.
    pub tuning_cents: f32,
    /// Global chroma (C..B), normalised to sum 1.
    pub chroma: [f32; 12],
}

/// Detect the key of a track. With stems the drums are excluded.
pub fn detect(audio: &clubcraft_audio::AudioBuffer, stems: Option<StemsRef>) -> Option<Key> {
    estimate(audio, stems, &KeyOptions::default()).map(|e| e.key)
}

pub(crate) fn detect_prepared(prep: &crate::Prepared) -> Option<Key> {
    let sig = harmonic_signal(&prep.mono, prep.stems);
    estimate_signal(&sig, prep.sr, prep.stems.is_none(), &KeyOptions::default()).map(|e| e.key)
}

fn harmonic_signal(mono: &[f32], stems: Option<StemsRef>) -> Vec<f32> {
    match stems {
        Some(s) => s.mix_mono([0.0, 1.0, 1.0, 0.5], mono.len()),
        None => mono.to_vec(),
    }
}

pub fn estimate(audio: &clubcraft_audio::AudioBuffer, stems: Option<StemsRef>, opts: &KeyOptions) -> Option<KeyEstimate> {
    let mono = audio.mono();
    let sig = harmonic_signal(&mono, stems);
    estimate_signal(&sig, audio.sample_rate as f64, stems.is_none(), opts)
}

const FMIN: f64 = 40.0;
const FMAX: f64 = 2100.0;
const BASS_SPLIT_MIDI: f64 = 48.0; // C3 = 130.8 Hz

/// Magnitude spectrogram (bins up to FMAX) of `x`.
struct Spec {
    bin_hz: f64,
    nb: usize,
    frames: Vec<Vec<f32>>,
}

fn spectrogram(x: &[f32], sr: f64) -> Spec {
    let n_fft = ((16384.0 * sr / 44100.0).round() as usize).next_power_of_two();
    let hop = n_fft / 4;
    let bin_hz = sr / n_fft as f64;
    let nb = ((FMAX * 1.03) / bin_hz).ceil() as usize + 2;
    let frames = dsp::stft_map(x, n_fft, hop, |_, s| s[..nb].iter().map(|c| c.norm()).collect::<Vec<f32>>());
    Spec { bin_hz, nb, frames }
}

/// Median filter each bin over time (keeps sustained, harmonic partials; removes transients).
fn harmonic_median(spec: &Spec, half: usize) -> Vec<Vec<f32>> {
    use rayon::prelude::*;
    let n = spec.frames.len();
    (0..n)
        .into_par_iter()
        .map(|i| {
            let a = i.saturating_sub(half);
            let b = (i + half + 1).min(n);
            let mut buf = Vec::with_capacity(b - a);
            (0..spec.nb)
                .map(|k| {
                    buf.clear();
                    buf.extend((a..b).map(|j| spec.frames[j][k]));
                    dsp::median_inplace(&mut buf)
                })
                .collect()
        })
        .collect()
}

fn estimate_tuning(frames: &[Vec<f32>], bin_hz: f64) -> f32 {
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    let lo = (100.0 / bin_hz) as usize;
    let hi = (FMAX / bin_hz) as usize;
    for f in frames.iter().step_by(2) {
        let mx = f[lo..hi].iter().cloned().fold(0.0f32, f32::max);
        if mx <= 1e-6 {
            continue;
        }
        let thr = mx * 0.1;
        for k in lo.max(1)..hi.min(f.len() - 1) {
            let v = f[k];
            if v > thr && v >= f[k - 1] && v > f[k + 1] {
                let off = dsp::parabolic(f[k - 1] as f64, v as f64, f[k + 1] as f64);
                let freq = (k as f64 + off) * bin_hz;
                let cents = 1200.0 * (freq / 440.0).log2();
                let dev = cents - 100.0 * (cents / 100.0).round();
                let ang = dev / 100.0 * 2.0 * std::f64::consts::PI;
                let w = v as f64;
                sx += w * ang.cos();
                sy += w * ang.sin();
            }
        }
    }
    (sy.atan2(sx) / (2.0 * std::f64::consts::PI) * 100.0) as f32
}

/// Per-frame (bass, treble) chroma.
fn chroma_frames(frames: &[Vec<f32>], bin_hz: f64, tuning_cents: f32) -> Vec<([f32; 12], [f32; 12], f32)> {
    let nb = frames.first().map(|f| f.len()).unwrap_or(0);
    // Precompute bin -> (pitch, weight) mapping.
    let mut map: Vec<(usize, usize, f32, bool)> = Vec::new(); // (bin, pitch class, weight, bass)
    for k in 1..nb {
        let f = k as f64 * bin_hz;
        if !(FMIN..=FMAX).contains(&f) {
            continue;
        }
        let p = 69.0 + 12.0 * (f / 440.0).log2() - tuning_cents as f64 / 100.0;
        let r = p.round();
        let d = p - r;
        let w = (std::f64::consts::PI * d).cos().powi(2);
        if w < 0.05 {
            continue;
        }
        let pc = (r as i64).rem_euclid(12) as usize;
        map.push((k, pc, w as f32, r < BASS_SPLIT_MIDI));
    }
    frames
        .iter()
        .map(|f| {
            let mut bass = [0.0f32; 12];
            let mut treb = [0.0f32; 12];
            let mut e = 0.0f32;
            for &(k, pc, w, is_bass) in &map {
                let v = f[k] * w;
                e += v;
                if is_bass {
                    bass[pc] += v;
                } else {
                    treb[pc] += v;
                }
            }
            (bass, treb, e)
        })
        .collect()
}

fn normalize_log(c: &[f32; 12]) -> [f32; 12] {
    let mx = c.iter().cloned().fold(0.0f32, f32::max);
    let mut o = [0.0f32; 12];
    if mx <= 1e-9 {
        return o;
    }
    for i in 0..12 {
        o[i] = (1.0 + 10.0 * c[i] / mx).ln();
    }
    let s: f32 = o.iter().sum();
    for v in &mut o {
        *v /= s.max(1e-9);
    }
    o
}

fn correlate(c: &[f32; 12], t: &[f32; 12], rot: usize) -> f32 {
    let mc = c.iter().sum::<f32>() / 12.0;
    let mt = t.iter().sum::<f32>() / 12.0;
    let (mut num, mut dc, mut dt) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let a = c[(i + rot) % 12] - mc;
        let b = t[i] - mt;
        num += a * b;
        dc += a * a;
        dt += b * b;
    }
    num / (dc * dt).sqrt().max(1e-12)
}

/// Best key for a chroma vector: (key, score, margin).
pub fn key_from_chroma(c: &[f32; 12], profile: Profile) -> (Key, f32, f32) {
    let (maj, min) = profile.templates();
    let mut all: Vec<(Key, f32)> = Vec::with_capacity(24);
    for t in 0..12 {
        all.push((Key::new(t as u8, false), correlate(c, &maj, t)));
        all.push((Key::new(t as u8, true), correlate(c, &min, t)));
    }
    all.sort_by(|a, b| b.1.total_cmp(&a.1));
    (all[0].0, all[0].1, all[0].1 - all[1].1)
}

/// Diagnostics: (tuning cents, treble chroma, bass chroma) accumulated over the track.
#[doc(hidden)]
pub fn debug_chroma(audio: &clubcraft_audio::AudioBuffer, stems: Option<StemsRef>) -> Option<(f32, [f32; 12], [f32; 12])> {
    let mono = audio.mono();
    let sig = harmonic_signal(&mono, stems);
    let spec = spectrogram(&sig, audio.sample_rate as f64);
    let frames = if stems.is_none() { harmonic_median(&spec, 4) } else { spec.frames.clone() };
    let tuning = estimate_tuning(&frames, spec.bin_hz);
    let ch = chroma_frames(&frames, spec.bin_hz, tuning);
    let emax = ch.iter().map(|c| c.2).fold(0.0f32, f32::max);
    let (mut tb, mut bb) = ([0.0f32; 12], [0.0f32; 12]);
    for (bass, treb, e) in &ch {
        if *e < emax * 0.02 {
            continue;
        }
        let w = e.sqrt();
        let b = normalize_log(bass);
        let t = normalize_log(treb);
        for i in 0..12 {
            tb[i] += w * t[i];
            bb[i] += w * b[i];
        }
    }
    let (st, sb): (f32, f32) = (tb.iter().sum(), bb.iter().sum());
    for i in 0..12 {
        tb[i] /= st.max(1e-9);
        bb[i] /= sb.max(1e-9);
    }
    Some((tuning, tb, bb))
}

pub(crate) fn estimate_signal(x: &[f32], sr: f64, hpss: bool, opts: &KeyOptions) -> Option<KeyEstimate> {
    let spec = spectrogram(x, sr);
    if spec.frames.len() < 8 {
        return None;
    }
    let frames = if hpss { harmonic_median(&spec, 4) } else { spec.frames.clone() };
    let tuning = estimate_tuning(&frames, spec.bin_hz);
    let ch = chroma_frames(&frames, spec.bin_hz, tuning);
    let emax = ch.iter().map(|c| c.2).fold(0.0f32, f32::max);
    if emax <= 1e-6 {
        return None;
    }
    let mut acc = [0.0f32; 12];
    for (bass, treb, e) in &ch {
        if *e < emax * 0.02 {
            continue;
        }
        let w = e.sqrt();
        let b = normalize_log(bass);
        let t = normalize_log(treb);
        for i in 0..12 {
            acc[i] += w * (t[i] + opts.bass_weight * b[i]);
        }
    }
    let s: f32 = acc.iter().sum();
    if s <= 0.0 {
        return None;
    }
    for v in &mut acc {
        *v /= s;
    }
    let (key, score, margin) = key_from_chroma(&acc, opts.profile);
    Some(KeyEstimate { key, score, margin, tuning_cents: tuning, chroma: acc })
}
