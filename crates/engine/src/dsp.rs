//! DSP building blocks shared by decks, mixer and effects: smoothing, interpolation, biquads,
//! the 3-band isolator EQ, the one-knob DJ filter, the master limiter and level meters.
//!
//! Everything here is allocation free after construction.

use std::f64::consts::PI;

/// Largest block the engine processes at once (frames). Device buffers are split into chunks of
/// at most this size, so every scratch buffer can be preallocated.
pub const MAX_BLOCK: usize = 256;

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn gain_to_db(g: f32) -> f32 {
    if g <= 1e-9 {
        -180.0
    } else {
        20.0 * g.log10()
    }
}

/// One-pole parameter smoother (exponential approach to `target`).
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    pub value: f32,
    pub target: f32,
    coef: f32,
}

impl Smoother {
    pub fn new(value: f32, time_s: f32, sample_rate: f32) -> Self {
        let mut s = Smoother { value, target: value, coef: 1.0 };
        s.set_time(time_s, sample_rate);
        s
    }

    pub fn set_time(&mut self, time_s: f32, sample_rate: f32) {
        self.coef = if time_s <= 0.0 { 1.0 } else { 1.0 - (-1.0 / (time_s * sample_rate)).exp() };
    }

    #[inline]
    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    pub fn reset(&mut self, v: f32) {
        self.value = v;
        self.target = v;
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        let d = self.target - self.value;
        if d.abs() < 1e-7 {
            self.value = self.target;
        } else {
            self.value += d * self.coef;
        }
        self.value
    }

    /// Advance `n` samples at once and return `(start, end)` values for a linear ramp.
    #[inline]
    pub fn advance(&mut self, n: usize) -> (f32, f32) {
        let start = self.value;
        let d = self.target - self.value;
        if d.abs() < 1e-6 {
            self.value = self.target;
        } else {
            self.value = self.target - d * (1.0 - self.coef).powi(n as i32);
        }
        (start, self.value)
    }

    pub fn settled(&self) -> bool {
        self.value == self.target
    }
}

/// 4-point, 3rd-order Hermite (Catmull-Rom) interpolation. `t` in 0..1 between `x0` and `x1`;
/// returns exactly `x0` at `t = 0`.
#[inline]
pub fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + x0
}

/// Transposed direct form II biquad (RBJ cookbook designs).
#[derive(Clone, Copy, Debug, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn from_coefs(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        Biquad {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn w(f: f64, q: f64, sr: f64) -> (f64, f64) {
        let w0 = 2.0 * PI * (f / sr).min(0.49);
        (w0.cos(), w0.sin() / (2.0 * q))
    }

    pub fn lowpass(f: f64, q: f64, sr: f64) -> Self {
        let (c, a) = Self::w(f, q, sr);
        Self::from_coefs((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0, 1.0 + a, -2.0 * c, 1.0 - a)
    }

    pub fn highpass(f: f64, q: f64, sr: f64) -> Self {
        let (c, a) = Self::w(f, q, sr);
        Self::from_coefs((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0, 1.0 + a, -2.0 * c, 1.0 - a)
    }

    pub fn allpass(f: f64, q: f64, sr: f64) -> Self {
        let (c, a) = Self::w(f, q, sr);
        Self::from_coefs(1.0 - a, -2.0 * c, 1.0 + a, 1.0 + a, -2.0 * c, 1.0 - a)
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

const BUTTERWORTH_Q: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Maximum knob value accepted internally: 1.0 = +6 dB is the user range; combined studio
/// lane + transition automation may reach 1.5 = +12 dB.
pub const EQ_KNOB_MAX: f32 = 1.5;

/// Maps an EQ knob (0 = kill, 0.5 = unity, 1 = +6 dB; up to [`EQ_KNOB_MAX`] = +12 dB) to a linear
/// gain. Below 0.5 the law is `(2k)²`.
#[inline]
pub fn eq_knob_gain(k: f32) -> f32 {
    let k = k.clamp(0.0, EQ_KNOB_MAX);
    if k <= 0.5 {
        let x = 2.0 * k;
        x * x
    } else {
        db_to_gain(12.0 * (k - 0.5))
    }
}

/// Inverse of [`eq_knob_gain`] (clamped to 0..[`EQ_KNOB_MAX`]).
#[inline]
pub fn eq_gain_to_knob(g: f32) -> f32 {
    if g <= 0.0 {
        0.0
    } else if g <= 1.0 {
        0.5 * g.sqrt()
    } else {
        (0.5 + gain_to_db(g) / 12.0).min(EQ_KNOB_MAX)
    }
}

/// Combine two EQ knob settings multiplicatively (gains add in dB) and return the knob value.
#[inline]
pub fn eq_combine(a: f32, b: f32) -> f32 {
    eq_gain_to_knob(eq_knob_gain(a) * eq_knob_gain(b))
}

#[derive(Clone, Copy, Debug, Default)]
struct Eq3Channel {
    lp1a: Biquad,
    lp1b: Biquad,
    ap2: Biquad,
    hp1a: Biquad,
    hp1b: Biquad,
    lp2a: Biquad,
    lp2b: Biquad,
    hp2a: Biquad,
    hp2b: Biquad,
}

impl Eq3Channel {
    fn new(low_hz: f64, high_hz: f64, sr: f64) -> Self {
        let q = BUTTERWORTH_Q;
        Eq3Channel {
            lp1a: Biquad::lowpass(low_hz, q, sr),
            lp1b: Biquad::lowpass(low_hz, q, sr),
            ap2: Biquad::allpass(high_hz, q, sr),
            hp1a: Biquad::highpass(low_hz, q, sr),
            hp1b: Biquad::highpass(low_hz, q, sr),
            lp2a: Biquad::lowpass(high_hz, q, sr),
            lp2b: Biquad::lowpass(high_hz, q, sr),
            hp2a: Biquad::highpass(high_hz, q, sr),
            hp2b: Biquad::highpass(high_hz, q, sr),
        }
    }

    #[inline]
    fn process(&mut self, x: f32, gl: f32, gm: f32, gh: f32) -> f32 {
        let low = self.ap2.process(self.lp1b.process(self.lp1a.process(x)));
        let rest = self.hp1b.process(self.hp1a.process(x));
        let mid = self.lp2b.process(self.lp2a.process(rest));
        let high = self.hp2b.process(self.hp2a.process(rest));
        gl * low + gm * mid + gh * high
    }
}

/// 3-band isolator EQ built from 4th-order Linkwitz-Riley crossovers. The low band is phase
/// compensated with the high crossover's allpass, so at unity the bands sum to a flat
/// (allpass) response; a band at 0 is a full kill (24 dB/oct skirts).
#[derive(Clone, Debug)]
pub struct Eq3 {
    ch: [Eq3Channel; 2],
    gains: [Smoother; 3],
}

impl Eq3 {
    pub const LOW_HZ: f64 = 220.0;
    pub const HIGH_HZ: f64 = 2800.0;

    pub fn new(sample_rate: f32) -> Self {
        let c = Eq3Channel::new(Self::LOW_HZ, Self::HIGH_HZ, sample_rate as f64);
        Eq3 { ch: [c, c], gains: [Smoother::new(1.0, 0.01, sample_rate); 3] }
    }

    /// Knob positions 0..1 (0 = kill, 0.5 = unity, 1 = +6 dB).
    pub fn set_knobs(&mut self, low: f32, mid: f32, high: f32) {
        self.gains[0].set(eq_knob_gain(low));
        self.gains[1].set(eq_knob_gain(mid));
        self.gains[2].set(eq_knob_gain(high));
    }

    /// Jump to the knob settings without smoothing (a track starting mid-automation).
    pub fn snap(&mut self) {
        for g in self.gains.iter_mut() {
            g.reset(g.target);
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for f in buf.chunks_exact_mut(2) {
            let gl = self.gains[0].next();
            let gm = self.gains[1].next();
            let gh = self.gains[2].next();
            f[0] = self.ch[0].process(f[0], gl, gm, gh);
            f[1] = self.ch[1].process(f[1], gl, gm, gh);
        }
    }
}

/// Topology-preserving-transform state variable filter (Simper). Stable under fast modulation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Svf {
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
    ic1: [f32; 2],
    ic2: [f32; 2],
}

/// Outputs of [`Svf::process`].
#[derive(Clone, Copy, Debug)]
pub struct SvfOut {
    pub lp: f32,
    pub bp: f32,
    pub hp: f32,
}

impl Svf {
    pub fn set(&mut self, cutoff: f32, q: f32, sr: f32) {
        let fc = cutoff.clamp(10.0, sr * 0.45);
        let g = (std::f32::consts::PI * fc / sr).tan();
        self.k = 1.0 / q.max(0.3);
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    #[inline]
    pub fn process(&mut self, c: usize, v0: f32) -> SvfOut {
        let v3 = v0 - self.ic2[c];
        let v1 = self.a1 * self.ic1[c] + self.a2 * v3;
        let v2 = self.ic2[c] + self.a2 * self.ic1[c] + self.a3 * v3;
        self.ic1[c] = 2.0 * v1 - self.ic1[c];
        self.ic2[c] = 2.0 * v2 - self.ic2[c];
        SvfOut { lp: v2, bp: v1, hp: v0 - self.k * v1 - v2 }
    }

    pub fn reset(&mut self) {
        self.ic1 = [0.0; 2];
        self.ic2 = [0.0; 2];
    }
}

/// Single-knob DJ filter: 0.5 = off, below = low-pass (down to ~60 Hz at 0), above = high-pass
/// (up to ~10 kHz at 1). 24 dB/oct (resonant SVF + Butterworth SVF), smoothed, and faded in
/// around the centre so there is no step when it engages.
#[derive(Clone, Debug)]
pub struct DjFilter {
    sr: f32,
    knob: Smoother,
    resonance: f32,
    s1: Svf,
    s2: Svf,
    counter: usize,
    wet: f32,
    hp: bool,
}

impl DjFilter {
    pub fn new(sr: f32) -> Self {
        DjFilter { sr, knob: Smoother::new(0.5, 0.03, sr), resonance: 0.3, s1: Svf::default(), s2: Svf::default(), counter: 0, wet: 0.0, hp: false }
    }

    /// Jump to the knob without smoothing.
    pub fn snap(&mut self) {
        self.knob.reset(self.knob.target);
    }

    pub fn set(&mut self, knob: f32) {
        self.knob.set(knob.clamp(0.0, 1.0));
    }

    /// 0..1 (0 = Butterworth, 1 = strongly resonant).
    pub fn set_resonance(&mut self, r: f32) {
        self.resonance = r.clamp(0.0, 1.0);
    }

    pub fn is_bypassed(&self) -> bool {
        self.knob.settled() && (self.knob.value - 0.5).abs() < 0.01
    }

    fn update(&mut self, k: f32) {
        let d = k - 0.5;
        self.wet = ((d.abs() - 0.01) / 0.05).clamp(0.0, 1.0);
        self.hp = d > 0.0;
        let cutoff = if self.hp {
            20.0 * (10_000.0f32 / 20.0).powf((d / 0.5).clamp(0.0, 1.0))
        } else {
            20_000.0 * (60.0f32 / 20_000.0).powf((-d / 0.5).clamp(0.0, 1.0))
        };
        let q = 0.707 + self.resonance * self.resonance * 4.0;
        self.s1.set(cutoff, q, self.sr);
        self.s2.set(cutoff, 0.707, self.sr);
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if self.is_bypassed() {
            if self.wet != 0.0 {
                self.wet = 0.0;
                self.s1.reset();
                self.s2.reset();
            }
            return;
        }
        for f in buf.chunks_exact_mut(2) {
            if self.counter == 0 {
                let k = self.knob.value;
                self.update(k);
                self.counter = 16;
            }
            self.counter -= 1;
            self.knob.next();
            for c in 0..2 {
                let x = f[c];
                let a = self.s1.process(c, x);
                let y1 = if self.hp { a.hp } else { a.lp };
                let b = self.s2.process(c, y1);
                let y = if self.hp { b.hp } else { b.lp };
                f[c] = x + (y - x) * self.wet;
            }
        }
    }
}

/// Peak + RMS level meter (linear amplitudes).
#[derive(Clone, Copy, Debug)]
pub struct Meter {
    pub peak: [f32; 2],
    ms: [f32; 2],
    rms_coef: f32,
    peak_fall: f32,
}

impl Meter {
    pub fn new(sr: f32) -> Self {
        Meter {
            peak: [0.0; 2],
            ms: [0.0; 2],
            rms_coef: 1.0 - (-1.0 / (0.3 * sr)).exp(),
            // ~ 20 dB/s fall per sample
            peak_fall: db_to_gain(-20.0 / sr),
        }
    }

    pub fn process(&mut self, buf: &[f32]) {
        for f in buf.chunks_exact(2) {
            for c in 0..2 {
                let a = f[c].abs();
                self.peak[c] = if a > self.peak[c] { a } else { self.peak[c] * self.peak_fall };
                self.ms[c] += (f[c] * f[c] - self.ms[c]) * self.rms_coef;
            }
        }
    }

    pub fn rms(&self) -> [f32; 2] {
        [self.ms[0].max(0.0).sqrt(), self.ms[1].max(0.0).sqrt()]
    }

    pub fn reset(&mut self) {
        self.peak = [0.0; 2];
        self.ms = [0.0; 2];
    }
}

/// Lookahead brick-wall limiter with true-peak (4x interpolated) detection and a final safety
/// clamp: the output never exceeds the ceiling.
///
/// Gain computer: required gain per sample → sliding minimum over the lookahead window → box
/// filter of the same length (smooth attack that reaches the required gain exactly when the
/// peak leaves the delay line) → exponential release.
#[derive(Clone, Debug)]
pub struct Limiter {
    pub enabled: bool,
    ceiling: f32,
    look: usize,
    delay: Vec<[f32; 2]>,
    req: Vec<f32>,
    hold: Vec<f32>,
    hold_sum: f64,
    pos: usize,
    hist: [[f32; 2]; 3],
    prev_ip: [f32; 2],
    release: f32,
    gain: f32,
    /// Smallest gain applied during the last block (for the UI "limiting" indicator).
    pub last_min_gain: f32,
}

impl Limiter {
    pub fn new(sr: f32) -> Self {
        let look = ((sr * 0.0015) as usize).max(8);
        Limiter {
            enabled: true,
            ceiling: db_to_gain(-0.3),
            look,
            delay: vec![[0.0; 2]; look],
            req: vec![1.0; look],
            hold: vec![1.0; look],
            hold_sum: look as f64,
            pos: 0,
            hist: [[0.0; 2]; 3],
            prev_ip: [0.0; 2],
            release: 1.0 - (-1.0 / (0.08 * sr)).exp(),
            gain: 1.0,
            last_min_gain: 1.0,
        }
    }

    pub fn set_ceiling_db(&mut self, db: f32) {
        self.ceiling = db_to_gain(db.min(0.0));
    }

    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }

    /// Latency in samples.
    pub fn latency(&self) -> usize {
        self.look + 1
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        let ceil = self.ceiling;
        let mut min_gain = 1.0f32;
        let n = self.look;
        // Detection margin for inter-sample peaks the 4x estimate misses and for gain changes
        // between neighbouring samples.
        let det_ceil = ceil * 0.985;
        for f in buf.chunks_exact_mut(2) {
            let x = [f[0], f[1]];
            // Sample s = hist[1]; its right-hand interval (hist[1], hist[2]) needs x as well.
            let mut ip_right = [0f32; 2];
            let mut peak = 0f32;
            for c in 0..2 {
                let (xm1, x0, x1, x2) = (self.hist[0][c], self.hist[1][c], self.hist[2][c], x[c]);
                let mut ip = 0f32;
                for t in [0.25f32, 0.5, 0.75] {
                    ip = ip.max(hermite(xm1, x0, x1, x2, t).abs());
                }
                ip_right[c] = ip;
                peak = peak.max(x0.abs()).max(ip).max(self.prev_ip[c]);
            }
            self.prev_ip = ip_right;
            let s = self.hist[1];
            self.hist[0] = self.hist[1];
            self.hist[1] = self.hist[2];
            self.hist[2] = x;
            let r = if peak > det_ceil { det_ceil / peak } else { 1.0 };
            let p = self.pos;
            self.req[p] = r;
            let mut h = 1.0f32;
            for v in &self.req {
                h = h.min(*v);
            }
            self.hold_sum += h as f64 - self.hold[p] as f64;
            self.hold[p] = h;
            let target = ((self.hold_sum / n as f64) as f32).min(1.0);
            if target < self.gain {
                self.gain = target;
            } else {
                self.gain += (target - self.gain) * self.release;
            }
            // delay n-1: sample s is output exactly when the box filter has fully absorbed r_s
            self.delay[p] = s;
            let d = self.delay[(p + 1) % n];
            self.pos = (p + 1) % n;
            if self.enabled {
                let g = self.gain;
                min_gain = min_gain.min(g);
                f[0] = (d[0] * g).clamp(-ceil, ceil);
                f[1] = (d[1] * g).clamp(-ceil, ceil);
            } else {
                f[0] = d[0].clamp(-1.0, 1.0);
                f[1] = d[1].clamp(-1.0, 1.0);
            }
        }
        // keep the running sum numerically honest
        self.hold_sum = self.hold.iter().map(|v| *v as f64).sum();
        self.last_min_gain = min_gain;
    }
}

/// Soft clipper for the headphone bus (never exceeds ±1).
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    if x.abs() <= 0.8 {
        x
    } else {
        let s = x.signum();
        let a = (x.abs() - 0.8) / 0.2;
        s * (0.8 + 0.2 * (a / (1.0 + a)))
    }
}

/// Equal-power crossfade weights for `t` in 0..1.
#[inline]
pub fn equal_power(t: f32) -> (f32, f32) {
    let a = t.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (a.cos(), a.sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f32, sr: f32, n: usize, amp: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = (2.0 * std::f32::consts::PI * f * i as f32 / sr).sin() * amp;
            v.push(s);
            v.push(s);
        }
        v
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt()
    }

    #[test]
    fn eq_unity_is_flat_and_low_kill_attenuates_60hz() {
        let sr = 44100.0;
        for f in [60.0, 1000.0, 8000.0] {
            let mut eq = Eq3::new(sr);
            eq.set_knobs(0.5, 0.5, 0.5);
            let mut x = sine(f, sr, 44100, 0.5);
            let r_in = rms(&x[20000..]);
            eq.process(&mut x);
            let r_out = rms(&x[20000..]);
            assert!((gain_to_db(r_out / r_in)).abs() < 0.1, "unity at {f} Hz: {} dB", gain_to_db(r_out / r_in));
        }
        let mut eq = Eq3::new(sr);
        eq.gains.iter_mut().for_each(|g| g.reset(1.0));
        eq.set_knobs(0.0, 0.5, 0.5);
        let mut x = sine(60.0, sr, 88200, 0.5);
        let r_in = rms(&x[44100..]);
        eq.process(&mut x);
        let att = gain_to_db(rms(&x[44100..]) / r_in);
        assert!(att < -40.0, "low kill at 60 Hz only {att} dB");
        // +6 dB at knob 1
        let mut eq = Eq3::new(sr);
        eq.set_knobs(0.5, 1.0, 0.5);
        let mut x = sine(1000.0, sr, 44100, 0.25);
        let r_in = rms(&x[20000..]);
        eq.process(&mut x);
        let g = gain_to_db(rms(&x[20000..]) / r_in);
        assert!((g - 6.0).abs() < 0.7, "mid boost {g}");
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let sr = 48000.0;
        let mut lim = Limiter::new(sr);
        lim.set_ceiling_db(0.0);
        // loud square-ish + sine bursts with intersample peaks, up to +12 dBFS
        let mut x = Vec::new();
        let mut seed = 1u32;
        for i in 0..96000 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 - 1.0;
            let env = if (i / 4800) % 2 == 0 { 4.0 } else { 0.3 };
            let s = (2.0 * std::f32::consts::PI * 11025.0 * i as f32 / sr + 0.7).sin() * env + noise * 0.5 * env;
            x.push(s);
            x.push(-s);
        }
        lim.process(&mut x);
        let mut peak = 0f32;
        for v in &x {
            peak = peak.max(v.abs());
        }
        assert!(peak <= 1.0, "peak {peak}");
        // true-peak check with 8x Hermite interpolation of the output
        let mut tp = 0f32;
        for c in 0..2 {
            let ch: Vec<f32> = x.iter().skip(c).step_by(2).copied().collect();
            for i in 1..ch.len() - 2 {
                for k in 0..8 {
                    tp = tp.max(hermite(ch[i - 1], ch[i], ch[i + 1], ch[i + 2], k as f32 / 8.0).abs());
                }
            }
        }
        assert!(tp <= 1.0, "true peak {tp}");
    }

    #[test]
    fn eq_knob_law_roundtrip_and_combine() {
        for k in [0.0f32, 0.1, 0.25, 0.5, 0.7, 1.0, 1.3] {
            assert!((eq_gain_to_knob(eq_knob_gain(k)) - k).abs() < 1e-5, "{k}");
        }
        assert_eq!(eq_combine(0.5, 0.5), 0.5);
        assert_eq!(eq_combine(0.0, 1.0), 0.0);
        // -12 dB (0.25) + +6 dB (1.0) = -6 dB
        assert!((gain_to_db(eq_knob_gain(eq_combine(0.25, 1.0))) + 6.04).abs() < 0.01);
        // +6 + +6 = +12 dB
        assert!((gain_to_db(eq_knob_gain(eq_combine(1.0, 1.0))) - 12.0).abs() < 0.01);
    }

    #[test]
    fn hermite_exact_at_zero() {
        assert_eq!(hermite(0.3, -0.7, 0.2, 0.9, 0.0), -0.7);
    }
}
