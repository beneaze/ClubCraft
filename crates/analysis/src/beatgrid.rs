//! Constant-tempo beat grid detection.
//!
//! 1. **Onset strength**: SuperFlux-style log-mel spectral flux (172 frames/s). With stems the
//!    drums stem is used (full band + kick band), otherwise the mix (full band + low band <200 Hz
//!    weighted x2, which locks onto the four-on-the-floor kick).
//! 2. **Tempo**: autocorrelation of the onset envelope scored as a comb (lags 1, 2 and 4 beats)
//!    times a log-Gaussian prior centred on 124 BPM; parabolic refinement.
//! 3. **Phase + precise tempo**: the envelope is *folded* modulo the beat period into a
//!    fine phase histogram (equivalent-time sampling gives sub-frame resolution). The track is
//!    split into 16-beat segments; their folded peak phases are unwrapped and fitted with a
//!    weighted, outlier-rejecting least-squares line whose slope corrects the period and whose
//!    intercept gives the phase. Iterated 3x, this is a global constant-grid fit accurate to
//!    ~1 ms over the whole track; segments without drums get no weight, so beatless intros
//!    and breakdowns do not disturb the grid (which simply extends through them).
//! 4. **Snapping**: the BPM snaps to the nearest integer or .5 when within 0.03 BPM and the
//!    folded beat profile is (almost) as sharp as with the free estimate.
//! 5. **Bar phase (downbeat)**: per-beat log band energies; for each of the 4 candidate bar
//!    phases we accumulate the bar-level (4 beats) and phrase-level (16 beats) novelty at beats
//!    of that phase — in dance music new elements enter, chords change and sections switch on
//!    bar 1. The first strong beat of the track adds a small prior (extended mixes start on a
//!    downbeat).

use crate::dsp;
use crate::frontend::{Spectral, N_MELS};
use clubcraft_core::BeatGrid;

/// Systematic latency of the onset envelope peak relative to the perceived beat (seconds,
/// added to detected beat times). Calibrated against DJ.Studio / Mixed In Key grids.
pub const BEAT_OFFSET_S: f64 = 0.0;

#[derive(Debug, Clone)]
pub struct BeatOptions {
    /// Tempo search range (BPM).
    pub min_bpm: f64,
    pub max_bpm: f64,
    /// The result is folded (x2 / /2) into this range.
    pub fold_min: f64,
    pub fold_max: f64,
    /// Centre (BPM) and width (octaves, 1 sigma) of the log-Gaussian tempo prior.
    pub prior_bpm: f64,
    pub prior_octaves: f64,
    /// Snap to integer / half BPM when the fit allows it.
    pub snap: bool,
}

impl Default for BeatOptions {
    fn default() -> Self {
        BeatOptions { min_bpm: 70.0, max_bpm: 180.0, fold_min: 85.0, fold_max: 175.0, prior_bpm: 124.0, prior_octaves: 0.6, snap: true }
    }
}

impl BeatOptions {
    /// Options for a known BPM range (e.g. from tags or the user): searches and folds into it.
    pub fn with_range(min_bpm: f64, max_bpm: f64) -> Self {
        let c = (min_bpm * max_bpm).sqrt();
        BeatOptions { min_bpm, max_bpm, fold_min: min_bpm, fold_max: max_bpm, prior_bpm: c, ..Default::default() }
    }
}

/// Detailed beat tracking result.
#[derive(Debug, Clone)]
pub struct GridEstimate {
    pub grid: BeatGrid,
    /// Sharpness of the folded beat profile (0 = no periodicity). >1.5 is a clear beat.
    pub confidence: f32,
    /// Bar phase scores (for diagnostics), index = beat offset of the downbeat from the first beat.
    pub bar_scores: [f32; 4],
}

/// Detect the beat grid of a track (mix only, or with stems for a more robust result).
pub fn detect(audio: &clubcraft_audio::AudioBuffer, stems: Option<crate::StemsRef>, opts: &BeatOptions) -> Option<BeatGrid> {
    let prep = crate::Prepared::new(audio, stems, false);
    estimate(&prep, opts).map(|e| e.grid)
}

/// Onset envelope used for beat tracking.
pub(crate) fn beat_envelope(spec: &Spectral, logmel: &[f32]) -> Vec<f32> {
    let full = spec.onset(logmel, 0..N_MELS);
    let low = spec.onset(logmel, spec.bands(0.0, 200.0));
    full.iter().zip(&low).map(|(a, b)| a + 2.0 * b).collect()
}

pub(crate) fn estimate(prep: &crate::Prepared, opts: &BeatOptions) -> Option<GridEstimate> {
    let (spec, logmel) = match &prep.drums {
        Some((s, l)) => (s, l),
        None => (&prep.mix, &prep.mix_log),
    };
    let env = beat_envelope(spec, logmel);
    let fps = spec.fps;
    if env.len() < (fps * 8.0) as usize {
        return None;
    }
    let bpm0 = tempo(&env, fps, opts)?;
    let (mut period, mut phase, mut conf) = refine(&env, fps, 60.0 * fps / bpm0);
    if conf <= 0.0 {
        return None;
    }
    let mut bpm = 60.0 * fps / period;
    if opts.snap {
        for step in [1.0, 0.5] {
            let snapped = (bpm / step).round() * step;
            if (snapped - bpm).abs() < 0.03 && (snapped - bpm).abs() > 1e-9 {
                let p2 = 60.0 * fps / snapped;
                let (ph2, c2) = global_phase(&env, p2, phase);
                if c2 >= conf * 0.985 {
                    period = p2;
                    phase = ph2;
                    conf = c2;
                    bpm = snapped;
                }
                break;
            } else if (snapped - bpm).abs() <= 1e-9 {
                bpm = snapped;
                break;
            }
        }
    }
    // First beat at or after t = 0 (in frames).
    let first = phase.rem_euclid(period);
    let beat_s = period / fps;
    let first_s = first / fps + BEAT_OFFSET_S;
    let first_s = if first_s < 0.0 { first_s + beat_s } else if first_s >= beat_s { first_s - beat_s } else { first_s };
    let (bar_off, bar_scores) = bar_phase(prep, &env, fps, first_s, beat_s);
    let grid = BeatGrid::new(bpm, first_s + bar_off as f64 * beat_s);
    Some(GridEstimate { grid, confidence: conf, bar_scores })
}

fn tempo(env: &[f32], fps: f64, opts: &BeatOptions) -> Option<f64> {
    let avg = dsp::moving_average(env, (fps * 1.0) as usize);
    let e: Vec<f32> = env.iter().zip(&avg).map(|(a, m)| (a - m).max(0.0)).collect();
    let max_lag = (60.0 * fps / opts.min_bpm * 4.0) as usize + 4;
    let ac = dsp::autocorr(&e, max_lag);
    let score = |bpm: f64| -> f64 {
        let tau = 60.0 * fps / bpm;
        let s = dsp::lerp_at(&ac, tau) as f64 + 0.5 * dsp::lerp_at(&ac, 2.0 * tau) as f64 + 0.5 * dsp::lerp_at(&ac, 4.0 * tau) as f64;
        let z = (bpm / opts.prior_bpm).log2() / opts.prior_octaves;
        s * (-0.5 * z * z).exp()
    };
    let step = 0.02;
    let n = ((opts.max_bpm - opts.min_bpm) / step) as usize + 1;
    let scores: Vec<f64> = (0..n).map(|i| score(opts.min_bpm + i as f64 * step)).collect();
    let (bi, _) = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    let mut bpm = opts.min_bpm + bi as f64 * step;
    if bi > 0 && bi + 1 < n {
        bpm += dsp::parabolic(scores[bi - 1], scores[bi], scores[bi + 1]) * step;
    }
    while bpm < opts.fold_min && bpm * 2.0 <= opts.fold_max * 1.02 {
        bpm *= 2.0;
    }
    while bpm > opts.fold_max && bpm / 2.0 >= opts.fold_min * 0.98 {
        bpm /= 2.0;
    }
    Some(bpm)
}

const FOLD_BINS: usize = 96;

/// Folded (beat-synchronous) mean profile of `env` over `range` for a grid (period, phase).
fn fold(env: &[f32], period: f64, phase: f64, range: std::ops::Range<usize>, out: &mut [f64; FOLD_BINS]) {
    let mut sum = [0.0f64; FOLD_BINS];
    let mut cnt = [0.0f64; FOLD_BINS];
    let nb = FOLD_BINS as f64;
    for n in range {
        let th = ((n as f64 - phase) / period).rem_euclid(1.0) * nb;
        let i0 = th.floor() as usize % FOLD_BINS;
        let f = th - th.floor();
        let i1 = (i0 + 1) % FOLD_BINS;
        let v = env[n] as f64;
        sum[i0] += v * (1.0 - f);
        cnt[i0] += 1.0 - f;
        sum[i1] += v * f;
        cnt[i1] += f;
    }
    for i in 0..FOLD_BINS {
        out[i] = if cnt[i] > 0.0 { sum[i] / cnt[i] } else { 0.0 };
    }
}

/// Peak phase (fraction of a beat, -0.5..0.5 around 0) and salience of a folded profile.
fn profile_peak(p: &[f64; FOLD_BINS]) -> (f64, f32) {
    // Light circular smoothing to stabilise the peak.
    let mut s = [0.0f64; FOLD_BINS];
    for i in 0..FOLD_BINS {
        let a = p[(i + FOLD_BINS - 1) % FOLD_BINS];
        let b = p[(i + 1) % FOLD_BINS];
        s[i] = 0.25 * a + 0.5 * p[i] + 0.25 * b;
    }
    let (im, &mx) = s.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
    let mean = s.iter().sum::<f64>() / FOLD_BINS as f64;
    if mean <= 1e-9 {
        return (0.0, 0.0);
    }
    let off = dsp::parabolic(s[(im + FOLD_BINS - 1) % FOLD_BINS], mx, s[(im + 1) % FOLD_BINS]);
    let mut th = (im as f64 + off) / FOLD_BINS as f64;
    if th >= 0.5 {
        th -= 1.0;
    }
    (th, ((mx - mean) / mean) as f32)
}

fn global_phase(env: &[f32], period: f64, phase: f64) -> (f64, f32) {
    let mut prof = [0.0; FOLD_BINS];
    fold(env, period, phase, 0..env.len(), &mut prof);
    let (th, sal) = profile_peak(&prof);
    let ph = phase + th * period;
    // Second pass centred on the found phase for a symmetric peak estimate.
    fold(env, period, ph, 0..env.len(), &mut prof);
    let (th2, sal2) = profile_peak(&prof);
    let _ = sal;
    (ph + th2 * period, sal2)
}

/// Iterative segment-phase regression. Returns (period, phase, confidence) in frames.
fn refine(env: &[f32], _fps: f64, period0: f64) -> (f64, f64, f32) {
    let mut period = period0;
    let (mut phase, _) = global_phase(env, period, 0.0);
    for iter in 0..4 {
        let seg_beats = if iter == 0 { 8.0 } else { 16.0 };
        let seg_len = (seg_beats * period).round() as usize;
        let mut pts: Vec<(f64, f64, f64)> = Vec::new(); // (beat index, theta, weight)
        let mut prof = [0.0; FOLD_BINS];
        let mut start = 0usize;
        while start + seg_len / 2 < env.len() {
            let end = (start + seg_len).min(env.len());
            fold(env, period, phase, start..end, &mut prof);
            let (th, sal) = profile_peak(&prof);
            let c = ((start + end) as f64 * 0.5 - phase) / period;
            if sal > 0.3 {
                pts.push((c, th, (sal as f64).powi(2) * (end - start) as f64 / seg_len as f64));
            }
            start = end;
        }
        if pts.len() < 3 {
            break;
        }
        // Unwrap sequentially (segments are in time order; the grid is already close).
        for i in 1..pts.len() {
            let prev = pts[i - 1].1;
            pts[i].1 += (prev - pts[i].1).round();
        }
        // Unwrap relative to the weighted circular mean (the grid is already close).
        let mut keep: Vec<bool> = vec![true; pts.len()];
        let mut fit = (0.0, 0.0);
        for _round in 0..3 {
            let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for (i, &(x, y, w)) in pts.iter().enumerate() {
                if !keep[i] {
                    continue;
                }
                sw += w;
                sx += w * x;
                sy += w * y;
                sxx += w * x * x;
                sxy += w * x * y;
            }
            if sw <= 0.0 {
                break;
            }
            let den = sw * sxx - sx * sx;
            let slope = if den.abs() > 1e-12 { (sw * sxy - sx * sy) / den } else { 0.0 };
            let icpt = (sy - slope * sx) / sw;
            fit = (icpt, slope);
            let mut changed = false;
            for (i, &(x, y, _)) in pts.iter().enumerate() {
                let r = y - (icpt + slope * x);
                let r = r - r.round();
                let k = r.abs() < 0.06;
                if k != keep[i] {
                    keep[i] = k;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let (icpt, slope) = fit;
        phase += icpt * period;
        period *= 1.0 + slope;
    }
    let (ph, conf) = global_phase(env, period, phase);
    (period, ph, conf)
}

/// Choose which of the 4 beats starting at `first_s` is beat 1 of the bar.
fn bar_phase(prep: &crate::Prepared, env: &[f32], fps: f64, first_s: f64, beat_s: f64) -> (usize, [f32; 4]) {
    let spec = &prep.mix;
    let logmel = &prep.mix_log;
    let dur = spec.n_frames as f64 / fps;
    let nbeats = ((dur - first_s) / beat_s).floor() as usize;
    if nbeats < 16 {
        return (0, [0.0; 4]);
    }
    const G: usize = 10;
    let per = N_MELS / G;
    // Per-beat band features (log magnitudes averaged over the beat).
    let mut feats = vec![[0.0f32; G]; nbeats];
    let mut strength = vec![0.0f32; nbeats];
    for (k, f) in feats.iter_mut().enumerate() {
        let a = ((first_s + k as f64 * beat_s) * fps).round() as usize;
        let b = (((first_s + (k + 1) as f64 * beat_s) * fps).round() as usize).min(spec.n_frames);
        if b <= a {
            continue;
        }
        for i in a..b {
            let fr = &logmel[i * N_MELS..(i + 1) * N_MELS];
            for g in 0..G {
                f[g] += fr[g * per..(g + 1) * per].iter().sum::<f32>() / per as f32;
            }
        }
        for v in f.iter_mut() {
            *v /= (b - a) as f32;
        }
        // Onset strength near the beat position.
        let c = ((first_s + k as f64 * beat_s) * fps).round() as isize;
        let mut m = 0.0f32;
        for d in -2..=2 {
            let i = c + d;
            if i >= 0 && (i as usize) < env.len() {
                m = m.max(env[i as usize]);
            }
        }
        strength[k] = m;
    }
    let novelty = |w: usize| -> Vec<f32> {
        let mut d = vec![0.0f32; nbeats];
        if nbeats < 2 * w {
            return d;
        }
        for k in w..=nbeats - w {
            let mut s = 0.0;
            for g in 0..G {
                let a: f32 = feats[k - w..k].iter().map(|f| f[g]).sum::<f32>() / w as f32;
                let b: f32 = feats[k..k + w].iter().map(|f| f[g]).sum::<f32>() / w as f32;
                s += (b - a).abs();
            }
            d[k] = s;
        }
        d
    };
    let d4 = novelty(4);
    let d16 = novelty(16);
    let mut sc4 = [0.0f32; 4];
    let mut sc16 = [0.0f32; 4];
    // Peaky novelty: only count local maxima (over +-2 beats) so each change votes for one phase.
    for (d, sc, r) in [(&d4, &mut sc4, 2usize), (&d16, &mut sc16, 2usize)] {
        for k in 0..nbeats {
            let lo = k.saturating_sub(r);
            let hi = (k + r + 1).min(nbeats);
            let v = d[k];
            if v > 0.0 && d[lo..hi].iter().all(|x| *x <= v) {
                sc[k % 4] += v;
            }
        }
        let tot: f32 = sc.iter().sum::<f32>().max(1e-9);
        for v in sc.iter_mut() {
            *v /= tot;
        }
    }
    // First strong beat prior.
    let thr = dsp::percentile(&strength, 75.0) * 0.4;
    let first_strong = strength.iter().position(|s| *s > thr).unwrap_or(0);
    let mut scores = [0.0f32; 4];
    for p in 0..4 {
        scores[p] = sc4[p] + sc16[p] + if first_strong % 4 == p { 0.15 } else { 0.0 };
    }
    let best = (0..4).max_by(|a, b| scores[*a].total_cmp(&scores[*b])).unwrap();
    (best, scores)
}
