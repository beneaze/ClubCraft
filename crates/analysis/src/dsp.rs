//! Small DSP toolbox shared by the analysis modules: STFT, mel filterbank, biquads, statistics.

use rayon::prelude::*;
use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

/// Periodic Hann window.
pub fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()) as f32).collect()
}

/// Number of centred STFT frames for `len` samples (frame `i` is centred on sample `i * hop`).
pub fn n_frames(len: usize, hop: usize) -> usize {
    if len == 0 {
        0
    } else {
        len.div_ceil(hop)
    }
}

/// Centred, Hann-windowed STFT evaluated in parallel. Frame `i` is centred on sample `i * hop`
/// (zero padded at the edges); `f(i, spectrum)` maps each spectrum (n_fft/2+1 bins) to a result.
/// Spectra are scaled so that a full-scale sine has a peak magnitude of ~0.5.
pub fn stft_map<T, F>(x: &[f32], n_fft: usize, hop: usize, f: F) -> Vec<T>
where
    T: Send,
    F: Fn(usize, &[Complex<f32>]) -> T + Sync,
{
    let frames = n_frames(x.len(), hop);
    let plan = RealFftPlanner::<f32>::new().plan_fft_forward(n_fft);
    let win = hann(n_fft);
    let scale = 2.0 / win.iter().sum::<f32>() * 0.5;
    let win: Vec<f32> = win.iter().map(|w| w * scale).collect();
    let half = (n_fft / 2) as isize;
    (0..frames)
        .into_par_iter()
        .map_init(
            || (plan.make_input_vec(), plan.make_output_vec(), plan.make_scratch_vec()),
            |(inp, out, scratch), i| {
                let start = (i * hop) as isize - half;
                if start >= 0 && start as usize + n_fft <= x.len() {
                    let s = start as usize;
                    for ((o, xv), w) in inp.iter_mut().zip(&x[s..s + n_fft]).zip(&win) {
                        *o = xv * w;
                    }
                } else {
                    for (j, o) in inp.iter_mut().enumerate() {
                        let idx = start + j as isize;
                        *o = if idx >= 0 && (idx as usize) < x.len() { x[idx as usize] * win[j] } else { 0.0 };
                    }
                }
                plan.process_with_scratch(inp, out, scratch).expect("fft");
                f(i, out)
            },
        )
        .collect()
}

/// Triangular mel filterbank (HTK mel scale) on magnitude spectra.
pub struct MelBank {
    pub filters: Vec<(usize, Vec<f32>)>,
    pub centers: Vec<f32>,
}

fn hz_to_mel(f: f64) -> f64 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_to_hz(m: f64) -> f64 {
    700.0 * (10f64.powf(m / 2595.0) - 1.0)
}

impl MelBank {
    pub fn new(n_fft: usize, sr: f64, n_mels: usize, fmin: f64, fmax: f64) -> MelBank {
        let bins = n_fft / 2 + 1;
        let bin_hz = sr / n_fft as f64;
        let (m0, m1) = (hz_to_mel(fmin), hz_to_mel(fmax.min(sr / 2.0)));
        let pts: Vec<f64> = (0..n_mels + 2).map(|i| mel_to_hz(m0 + (m1 - m0) * i as f64 / (n_mels + 1) as f64)).collect();
        let mut filters = Vec::with_capacity(n_mels);
        let mut centers = Vec::with_capacity(n_mels);
        for m in 0..n_mels {
            let (lo, c, hi) = (pts[m], pts[m + 1], pts[m + 2]);
            // Make sure every filter covers at least one bin (low mel bands are narrower than a bin).
            let lo_b = ((lo / bin_hz).floor() as usize).min(bins - 1);
            let hi_b = ((hi / bin_hz).ceil() as usize).min(bins - 1);
            let mut w = Vec::new();
            for b in lo_b..=hi_b {
                let f = b as f64 * bin_hz;
                let v = if f <= c { (f - lo) / (c - lo) } else { (hi - f) / (hi - c) };
                w.push(v.clamp(0.0, 1.0) as f32);
            }
            if w.iter().all(|v| *v == 0.0) {
                let nb = ((c / bin_hz).round() as usize).clamp(lo_b, hi_b);
                w[nb - lo_b] = 1.0;
            }
            let s: f32 = w.iter().sum();
            for v in &mut w {
                *v /= s.max(1e-9);
            }
            filters.push((lo_b, w));
            centers.push(c as f32);
        }
        MelBank { filters, centers }
    }

    pub fn apply(&self, spec: &[Complex<f32>], out: &mut [f32]) {
        for ((start, w), o) in self.filters.iter().zip(out.iter_mut()) {
            let mut acc = 0.0f32;
            for (j, wv) in w.iter().enumerate() {
                acc += wv * spec[start + j].norm();
            }
            *o = acc;
        }
    }
}

/// RBJ cookbook biquad (direct form I, f64 state for low-frequency precision).
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    fn from(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Biquad {
        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }
    pub fn lowpass(fc: f64, q: f64, sr: f64) -> Biquad {
        let w = 2.0 * std::f64::consts::PI * fc / sr;
        let (s, c) = w.sin_cos();
        let a = s / (2.0 * q);
        Biquad::from((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + a, -2.0 * c, 1.0 - a)
    }
    pub fn highpass(fc: f64, q: f64, sr: f64) -> Biquad {
        let w = 2.0 * std::f64::consts::PI * fc / sr;
        let (s, c) = w.sin_cos();
        let a = s / (2.0 * q);
        Biquad::from((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + a, -2.0 * c, 1.0 - a)
    }
    /// Raw coefficients (b0, b1, b2, a1, a2) with a0 = 1.
    pub fn raw(b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) -> Biquad {
        Biquad { b0, b1, b2, a1, a2, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }
    #[inline]
    pub fn tick(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// 4th-order Linkwitz-Riley style filter (two cascaded Butterworth biquads).
pub fn lr4(x: &[f32], fc: f64, sr: f64, high: bool) -> Vec<f32> {
    let q = std::f64::consts::FRAC_1_SQRT_2;
    let mk = || if high { Biquad::highpass(fc, q, sr) } else { Biquad::lowpass(fc, q, sr) };
    let (mut a, mut b) = (mk(), mk());
    x.iter().map(|v| b.tick(a.tick(*v as f64)) as f32).collect()
}

/// Mono downmix of a stereo pair.
pub fn mono(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).map(|(a, b)| 0.5 * (a + b)).collect()
}

/// Percentile (0..=100) of a slice (copies + sorts).
pub fn percentile(x: &[f32], p: f64) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    let mut v: Vec<f32> = x.iter().copied().filter(|v| v.is_finite()).collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let idx = ((p / 100.0) * (v.len() - 1) as f64).round() as usize;
    v[idx.min(v.len() - 1)]
}

pub fn mean(x: &[f32]) -> f32 {
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f32>() / x.len() as f32
    }
}

/// Centred moving average with window `w` (samples).
pub fn moving_average(x: &[f32], w: usize) -> Vec<f32> {
    let n = x.len();
    if n == 0 {
        return vec![];
    }
    let h = w / 2;
    let mut pre = vec![0.0f64; n + 1];
    for i in 0..n {
        pre[i + 1] = pre[i] + x[i] as f64;
    }
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(h);
            let b = (i + h + 1).min(n);
            ((pre[b] - pre[a]) / (b - a) as f64) as f32
        })
        .collect()
}

/// Linear interpolation into `x` at fractional index `pos` (0 outside).
#[inline]
pub fn lerp_at(x: &[f32], pos: f64) -> f32 {
    if pos < 0.0 {
        return 0.0;
    }
    let i = pos.floor() as usize;
    if i + 1 >= x.len() {
        return if i < x.len() { x[i] } else { 0.0 };
    }
    let f = (pos - i as f64) as f32;
    x[i] * (1.0 - f) + x[i + 1] * f
}

/// Parabolic peak interpolation around index `i`; returns the fractional offset in -0.5..0.5.
pub fn parabolic(ym: f64, y0: f64, yp: f64) -> f64 {
    let d = ym - 2.0 * y0 + yp;
    if d.abs() < 1e-12 {
        0.0
    } else {
        (0.5 * (ym - yp) / d).clamp(-0.5, 0.5)
    }
}

/// Circular autocorrelation-free (zero padded) autocorrelation via FFT, normalised so r[0] = 1.
pub fn autocorr(x: &[f32], max_lag: usize) -> Vec<f32> {
    let n = x.len();
    let size = (2 * n).next_power_of_two();
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(size);
    let inv = planner.plan_fft_inverse(size);
    let mut inp = fwd.make_input_vec();
    inp[..n].copy_from_slice(x);
    let mut spec = fwd.make_output_vec();
    fwd.process(&mut inp, &mut spec).expect("fft");
    for c in spec.iter_mut() {
        *c = Complex::new(c.norm_sqr(), 0.0);
    }
    let mut out = inv.make_output_vec();
    inv.process(&mut spec, &mut out).expect("ifft");
    let r0 = out[0].max(1e-12);
    out.truncate(max_lag.min(n));
    for v in out.iter_mut() {
        *v /= r0;
    }
    out
}

/// Median of a small buffer (destroys order).
pub fn median_inplace(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let m = v.len() / 2;
    let (_, x, _) = v.select_nth_unstable_by(m, |a, b| a.total_cmp(b));
    *x
}
