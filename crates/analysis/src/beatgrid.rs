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
pub const BEAT_OFFSET_S: f64 = -0.003;

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
    let cands = tempo_candidates(&env, fps, opts);
    let bpm0 = *cands.first()?;
    let (mut period, mut phase, mut conf) = refine(&env, fps, 60.0 * fps / bpm0);
    log::debug!("tempo: ac {bpm0:.3} -> refined {:.4} (conf {conf:.2})", 60.0 * fps / period);
    // Weak periodicity: also refine the runner-up tempo hypotheses and keep the sharpest grid.
    if conf < 2.0 {
        let prior = |b: f64| {
            let z = (b / opts.prior_bpm).log2() / (2.0 * opts.prior_octaves);
            (-0.5 * z * z).exp() as f32
        };
        let mut best_score = conf * prior(60.0 * fps / period);
        for &b in cands.iter().skip(1) {
            let (p2, ph2, c2) = refine(&env, fps, 60.0 * fps / b);
            let sc = c2 * prior(60.0 * fps / p2);
            log::debug!("tempo alternative: {b:.3} -> {:.4} (conf {c2:.2}, score {sc:.2} vs {best_score:.2})", 60.0 * fps / p2);
            if sc > best_score {
                best_score = sc;
                period = p2;
                phase = ph2;
                conf = c2;
            }
        }
    }
    if conf <= 0.0 {
        return None;
    }
    // On-beat vs off-beat: the folded pulse may have locked onto the off-beat (open hats,
    // off-beat bass). Kick body / claps (150 Hz - 4 kHz) sit on the beat, hats (> 4 kHz)
    // between beats.
    if onbeat_score(spec, logmel, period, phase) < 0.0 {
        phase += 0.5 * period;
    }
    let mut bpm = 60.0 * fps / period;
    if opts.snap {
        for step in [1.0, 0.5] {
            let snapped = (bpm / step).round() * step;
            if (snapped - bpm).abs() < 0.03 && (snapped - bpm).abs() > 1e-9 {
                let p2 = 60.0 * fps / snapped;
                let (ph2, c2) = near_phase(&env, p2, phase);
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

/// log(on-beat / half-beat) folded onset strength: + kick/clap bands, - hats band.
fn onbeat_score(spec: &Spectral, logmel: &[f32], period: f64, phase: f64) -> f64 {
    let bands = [(spec.bands(150.0, 600.0), 1.0), (spec.bands(600.0, 4000.0), 1.0), (spec.bands(4000.0, 20000.0), -1.0)];
    let mut score = 0.0;
    for (b, w) in bands {
        let env = spec.onset(logmel, b);
        let mut prof = [0.0; FOLD_BINS];
        fold(&env, period, phase, 0..env.len(), &mut prof);
        let at = |c: usize| (0..5).map(|d| prof[(c + FOLD_BINS + d - 2) % FOLD_BINS]).fold(0.0, f64::max).max(1e-9);
        score += w * (at(0) / at(FOLD_BINS / 2)).ln();
    }
    score
}

/// Tempo hypotheses (BPM, folded into the fold range), best first: local maxima of an
/// autocorrelation comb (1, 2, 4 and 8 beats) weighted by a log-Gaussian prior.
fn tempo_candidates(env: &[f32], fps: f64, opts: &BeatOptions) -> Vec<f64> {
    let avg = dsp::moving_average(env, (fps * 1.0) as usize);
    let e: Vec<f32> = env.iter().zip(&avg).map(|(a, m)| (a - m).max(0.0)).collect();
    let max_lag = (60.0 * fps / opts.min_bpm * 8.0) as usize + 4;
    let ac = dsp::autocorr(&e, max_lag);
    let score = |bpm: f64| -> f64 {
        let tau = 60.0 * fps / bpm;
        let a = |m: f64| dsp::lerp_at(&ac, m * tau) as f64;
        let s = a(1.0) + 0.5 * a(2.0) + 0.5 * a(4.0) + 0.25 * a(8.0);
        let z = (bpm / opts.prior_bpm).log2() / opts.prior_octaves;
        s * (-0.5 * z * z).exp()
    };
    let step = 0.02;
    let n = ((opts.max_bpm - opts.min_bpm) / step) as usize + 1;
    let scores: Vec<f64> = (0..n).map(|i| score(opts.min_bpm + i as f64 * step)).collect();
    let mut peaks: Vec<(f64, f64)> = Vec::new();
    for i in 1..n.saturating_sub(1) {
        if scores[i] > scores[i - 1] && scores[i] >= scores[i + 1] && scores[i] > 0.0 {
            let bpm = opts.min_bpm + (i as f64 + dsp::parabolic(scores[i - 1], scores[i], scores[i + 1])) * step;
            peaks.push((bpm, scores[i]));
        }
    }
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out: Vec<f64> = Vec::new();
    for (mut bpm, _) in peaks {
        while bpm < opts.fold_min && bpm * 2.0 <= opts.fold_max * 1.02 {
            bpm *= 2.0;
        }
        while bpm > opts.fold_max && bpm / 2.0 >= opts.fold_min * 0.98 {
            bpm /= 2.0;
        }
        if out.iter().all(|b| (b / bpm - 1.0).abs() > 0.01) {
            out.push(bpm);
        }
        if out.len() >= 4 {
            break;
        }
    }
    out
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

/// Like [`global_phase`] but only searches the profile peak within a quarter beat of `phase`
/// (keeps an on-beat decision). Salience is that of the whole profile.
fn near_phase(env: &[f32], period: f64, phase: f64) -> (f64, f32) {
    let mut prof = [0.0; FOLD_BINS];
    fold(env, period, phase, 0..env.len(), &mut prof);
    let q = FOLD_BINS / 4;
    let mut best = (0usize, f64::MIN);
    for d in 0..(2 * q) {
        let i = (FOLD_BINS + d - q) % FOLD_BINS;
        if prof[i] > best.1 {
            best = (i, prof[i]);
        }
    }
    let i = best.0;
    let off = dsp::parabolic(prof[(i + FOLD_BINS - 1) % FOLD_BINS], prof[i], prof[(i + 1) % FOLD_BINS]);
    let mut th = (i as f64 + off) / FOLD_BINS as f64;
    if th >= 0.5 {
        th -= 1.0;
    }
    let mean = prof.iter().sum::<f64>() / FOLD_BINS as f64;
    let mx = prof.iter().cloned().fold(f64::MIN, f64::max);
    (phase + th * period, if mean > 0.0 { ((mx - mean) / mean) as f32 } else { 0.0 })
}

/// Iterative segment-phase regression. Returns (period, phase, confidence) in frames.
fn refine(env: &[f32], _fps: f64, period0: f64) -> (f64, f64, f32) {
    use std::f64::consts::PI;
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
        // Robust initial line: maximise the circular coherence of the segment phases over a
        // grid of tempo corrections. Angles are doubled because a segment's strongest pulse
        // may be the off-beat (bass / open hats) instead of the kick; the on/off-beat choice is
        // made later. Then every segment phase is unwrapped (modulo half a beat) to that line
        // and refined by outlier-rejecting weighted least squares.
        let coh = |eps: f64| -> (f64, f64) {
            let (mut re, mut im) = (0.0, 0.0);
            for &(x, y, w) in &pts {
                let a = 4.0 * PI * (y - eps * x);
                re += w * a.cos();
                im += w * a.sin();
            }
            (re * re + im * im, im.atan2(re) / (4.0 * PI))
        };
        let range = if iter == 0 { 0.004 } else { 0.0005 };
        let steps = 400;
        let mut best = (0.0, f64::MIN, 0.0);
        for i in 0..=steps {
            let eps = -range + 2.0 * range * i as f64 / steps as f64;
            let (c, ph) = coh(eps);
            if c > best.1 {
                best = (eps, c, ph);
            }
        }
        for p in pts.iter_mut() {
            let pred = best.2 + best.0 * p.0;
            p.1 += ((pred - p.1) * 2.0).round() * 0.5;
        }
        let mut keep: Vec<bool> = vec![true; pts.len()];
        let mut fit = (best.2, best.0);
        for _round in 0..4 {
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
                let k = r.abs() < 0.05;
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
        log::trace!("refine iter {iter}: {} segs, icpt {icpt:.4} slope {slope:.6}", pts.len());
        phase += icpt * period;
        period *= 1.0 + slope;
    }
    // Final global fit: scan the tempo for the sharpest whole-track folded beat profile.
    let (mut ph, mut conf) = global_phase(env, period, phase);
    let base = period;
    for i in -30..=30 {
        let p = base * (1.0 + i as f64 * 2e-5);
        let (ph2, c2) = global_phase(env, p, ph);
        if c2 > conf {
            conf = c2;
            ph = ph2;
            period = p;
        }
    }
    (period, ph, conf)
}

/// Number of bar-phase evidence features (see [`phase_features`]).
pub(crate) const N_PHASE_FEATS: usize = 12;

/// Evidence for each of the 4 candidate bar phases (beat offset of the downbeat from the first
/// beat at/after 0). Each feature row is normalised to sum 1 over the candidates.
pub(crate) fn phase_features(prep: &crate::Prepared, env: &[f32], fps: f64, first_s: f64, beat_s: f64) -> Vec<[f32; 4]> {
    let spec = &prep.mix;
    let logmel = &prep.mix_log;
    let dur = spec.n_frames as f64 / fps;
    let nbeats = ((dur - first_s) / beat_s).floor().max(0.0) as usize;
    let mut out = vec![[0.25f32; 4]; N_PHASE_FEATS];
    if nbeats < 16 {
        return out;
    }
    const G: usize = 10;
    let per = N_MELS / G;
    let frame_of = |t: f64| ((t * fps).round().max(0.0) as usize).min(spec.n_frames);
    // Per-beat band features (log magnitudes averaged over the beat).
    let mut feats = vec![[0.0f32; G]; nbeats];
    let mut level = vec![0.0f32; nbeats];
    for (k, f) in feats.iter_mut().enumerate() {
        let a = frame_of(first_s + k as f64 * beat_s);
        let b = frame_of(first_s + (k + 1) as f64 * beat_s);
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
        level[k] = f.iter().sum::<f32>() / G as f32;
    }
    // Onset strength of a band near each beat.
    let near = |e: &[f32], k: usize| -> f32 {
        let c = frame_of(first_s + k as f64 * beat_s) as isize;
        (-2..=2).filter_map(|d| e.get((c + d).max(0) as usize).copied()).fold(0.0f32, f32::max)
    };
    let hi_env = spec.onset(logmel, spec.bands(4000.0, 20000.0));
    let lo_env = spec.onset(logmel, spec.bands(0.0, 150.0));
    let mid_env = spec.onset(logmel, spec.bands(150.0, 600.0));
    let strength: Vec<f32> = (0..nbeats).map(|k| near(env, k)).collect();
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
    let peaks = |d: &[f32], r: usize| -> Vec<f32> {
        (0..nbeats)
            .map(|k| {
                let lo = k.saturating_sub(r);
                let hi = (k + r + 1).min(nbeats);
                if d[k] > 0.0 && d[lo..hi].iter().all(|x| *x <= d[k]) { d[k] } else { 0.0 }
            })
            .collect()
    };
    let p4 = peaks(&d4, 2);
    let p16 = peaks(&d16, 2);
    let by_phase = |v: &[f32]| -> [f32; 4] {
        let mut s = [0.0f32; 4];
        for (k, x) in v.iter().enumerate() {
            s[k % 4] += x;
        }
        s
    };
    // Comb over 32-beat phrases: best phrase offset within each bar phase class.
    let comb32 = |v: &[f32]| -> [f32; 4] {
        let mut c = [0.0f32; 32];
        for (k, x) in v.iter().enumerate() {
            c[k % 32] += x * x;
        }
        let mut s = [0.0f32; 4];
        for (q, x) in c.iter().enumerate() {
            s[q % 4] = s[q % 4].max(*x);
        }
        s
    };
    let thr = dsp::percentile(&strength, 75.0) * 0.4;
    let first_strong = strength.iter().position(|s| *s > thr).unwrap_or(0);
    let mut first = [0.0f32; 4];
    first[first_strong % 4] = 1.0;
    // First beat with music (mean square above -65 dBFS), allowing a beat up to 30 ms before 0.
    let sr = prep.sr;
    let beat_level = |t: f64| -> f32 {
        let a = (t.max(0.0) * sr) as usize;
        let b = (((t + beat_s) * sr) as usize).min(prep.mono.len());
        if b <= a {
            return -200.0;
        }
        let ms = prep.mono[a..b].iter().map(|v| (v * v) as f64).sum::<f64>() / (b - a) as f64;
        (10.0 * ms.max(1e-30).log10()) as f32
    };
    let start_k: i64 = if first_s - beat_s >= -0.03 { -1 } else { 0 };
    let mut music = [0.0f32; 4];
    for k in start_k..(nbeats as i64) {
        if beat_level(first_s + k as f64 * beat_s) > -65.0 {
            music[k.rem_euclid(4) as usize] = 1.0;
            break;
        }
    }
    let rows: [[f32; 4]; N_PHASE_FEATS] = [
        by_phase(&p4),
        by_phase(&p16),
        first,
        comb32(&p4),
        comb32(&p16),
        by_phase(&(0..nbeats).map(|k| near(&hi_env, k)).collect::<Vec<_>>()),
        by_phase(&(0..nbeats).map(|k| near(&lo_env, k)).collect::<Vec<_>>()),
        by_phase(&(0..nbeats).map(|k| near(&mid_env, k)).collect::<Vec<_>>()),
        by_phase(&level),
        by_phase(&p4.iter().map(|x| x * x).collect::<Vec<_>>()),
        music,
        [1.0, 0.0, 0.0, 0.0],
    ];
    for (o, r) in out.iter_mut().zip(rows) {
        let tot: f32 = r.iter().sum();
        if tot > 0.0 {
            for p in 0..4 {
                o[p] = r[p] / tot;
            }
        }
    }
    out
}

/// Weights of the phase features (log-odds style linear combination).
const PHASE_WEIGHTS: [f32; N_PHASE_FEATS] = [0.0, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.01];

/// Choose which of the 4 beats starting at `first_s` is beat 1 of the bar.
fn bar_phase(prep: &crate::Prepared, env: &[f32], fps: f64, first_s: f64, beat_s: f64) -> (usize, [f32; 4]) {
    let f = phase_features(prep, env, fps, first_s, beat_s);
    let mut scores = [0.0f32; 4];
    for (row, w) in f.iter().zip(PHASE_WEIGHTS) {
        for p in 0..4 {
            scores[p] += w * row[p];
        }
    }
    let best = (0..4).max_by(|a, b| scores[*a].total_cmp(&scores[*b])).unwrap();
    (best, scores)
}

/// Diagnostics: (first beat time, beat length, phase feature matrix) for a track.
#[doc(hidden)]
pub fn debug_phase_features(audio: &clubcraft_audio::AudioBuffer) -> Option<(f64, f64, Vec<[f32; 4]>)> {
    let prep = crate::Prepared::new(audio, None, false);
    let est = estimate(&prep, &BeatOptions::default())?;
    let g = est.grid;
    let beat_s = g.beat_len();
    let first_s = g.downbeat.rem_euclid(beat_s);
    let env = beat_envelope(&prep.mix, &prep.mix_log);
    Some((first_s, beat_s, phase_features(&prep, &env, prep.mix.fps, first_s, beat_s)))
}

/// Diagnostics: folded onset strength of 5 bands (all, <150 Hz, 150-600, 600-4k, >4k) at the
/// grid's beat positions and half way between beats.
#[doc(hidden)]
pub fn debug_pulse_profile(audio: &clubcraft_audio::AudioBuffer, grid: &BeatGrid) -> Vec<[f32; 2]> {
    let prep = crate::Prepared::new(audio, None, false);
    let spec = &prep.mix;
    let fps = spec.fps;
    let bands = [0..N_MELS, spec.bands(0.0, 150.0), spec.bands(150.0, 600.0), spec.bands(600.0, 4000.0), spec.bands(4000.0, 20000.0)];
    let period = grid.beat_len() * fps;
    let phase = (grid.downbeat - BEAT_OFFSET_S) * fps;
    bands
        .iter()
        .map(|b| {
            let env = spec.onset(&prep.mix_log, b.clone());
            let mut prof = [0.0; FOLD_BINS];
            fold(&env, period, phase, 0..env.len(), &mut prof);
            let mean = prof.iter().sum::<f64>() / FOLD_BINS as f64;
            let at = |c: usize| (0..5).map(|d| prof[(c + FOLD_BINS + d - 2) % FOLD_BINS]).fold(0.0, f64::max) / mean.max(1e-9);
            [at(0) as f32, at(FOLD_BINS / 2) as f32]
        })
        .collect()
}
