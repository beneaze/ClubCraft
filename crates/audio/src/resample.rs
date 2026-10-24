//! Offline band-limited resampling (windowed-sinc, Kaiser window, 64 taps per side).

use crate::decode::AudioBuffer;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let y = x * x / 4.0;
    for k in 1..50 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

const HALF: i64 = 64;
const TAPS: usize = 2 * HALF as usize;
/// Kernel phases for rates without a small common grid (interpolated between).
const PHASES: usize = 512;
/// Up to this many exact phases are tabulated (48 kHz → 44.1 kHz needs 147).
const MAX_EXACT_PHASES: u64 = 4096;

/// The windowed-sinc kernel of one conversion, tabulated by sub-sample phase.
struct Kernel {
    from: u64,
    to: u64,
    /// Exact phases: output `i` uses phase `(i * from mod to) / step`, no interpolation.
    exact: bool,
    step: u64,
    phases: usize,
    table: Vec<f32>,
}

impl Kernel {
    fn new(from: u32, to: u32) -> Kernel {
        let (from, to) = (from as u64, to as u64);
        let g = gcd(from, to);
        let exact = to / g <= MAX_EXACT_PHASES;
        let phases = if exact { (to / g) as usize } else { PHASES };
        let ratio = to as f64 / from as f64;
        let cutoff = ratio.min(1.0) * 0.97; // relative to input Nyquist
        let beta = 9.0;
        let i0b = bessel_i0(beta);
        // the interpolated table has one more phase, so phase p + 1 always exists
        let rows = if exact { phases } else { phases + 1 };
        let mut table = vec![0f32; rows * TAPS];
        for p in 0..rows {
            let frac = p as f64 / phases as f64;
            for k in 0..TAPS {
                let x = (k as i64 - HALF + 1) as f64 - frac;
                let s = if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * x * cutoff).sin() / (std::f64::consts::PI * x * cutoff) };
                let w = {
                    let t = x / HALF as f64;
                    if t.abs() >= 1.0 { 0.0 } else { bessel_i0(beta * (1.0 - t * t).sqrt()) / i0b }
                };
                table[p * TAPS + k] = (s * w * cutoff) as f32;
            }
        }
        Kernel { from, to, exact, step: g, phases, table }
    }

    fn out_len(&self, frames: usize) -> usize {
        ((frames as f64) * self.to as f64 / self.from as f64).round() as usize
    }

    /// Output samples `first..first + out.len()` of `input`.
    fn run(&self, input: &[f32], first: usize, out: &mut [f32]) {
        let n = input.len() as i64;
        let mut coef = [0f32; TAPS];
        for (j, o) in out.iter_mut().enumerate() {
            let i = (first + j) as u64;
            let num = i * self.from;
            let base = (num / self.to) as i64;
            let k: &[f32] = if self.exact {
                let p = ((num % self.to) / self.step) as usize;
                &self.table[p * TAPS..(p + 1) * TAPS]
            } else {
                let pf = (num % self.to) as f64 / self.to as f64 * self.phases as f64;
                let p0 = pf.floor() as usize;
                let t = (pf - p0 as f64) as f32;
                let (k0, k1) = (&self.table[p0 * TAPS..(p0 + 1) * TAPS], &self.table[(p0 + 1) * TAPS..(p0 + 2) * TAPS]);
                for (c, (a, b)) in coef.iter_mut().zip(k0.iter().zip(k1)) {
                    *c = a + (b - a) * t;
                }
                &coef
            };
            let start = base - HALF + 1;
            *o = if start >= 0 && start + TAPS as i64 <= n {
                dot(&input[start as usize..start as usize + TAPS], k)
            } else {
                // the edges: taps outside the input read silence
                let mut acc = 0f32;
                for (t, c) in k.iter().enumerate() {
                    let idx = start + t as i64;
                    if idx >= 0 && idx < n {
                        acc += input[idx as usize] * c;
                    }
                }
                acc
            };
        }
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// Eight running sums, so the compiler can use SIMD (one sum would fix the order of additions).
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    for (x, y) in a.as_chunks::<8>().0.iter().zip(b.as_chunks::<8>().0) {
        for j in 0..8 {
            acc[j] += x[j] * y[j];
        }
    }
    acc.iter().sum()
}

/// Resample one channel from `from` Hz to `to` Hz.
pub fn resample_channel(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let k = Kernel::new(from, to);
    let mut out = vec![0f32; k.out_len(input.len())];
    k.run(input, 0, &mut out);
    out
}

/// Resample both channels, split over the CPU cores: a track is decoded while the user waits
/// for it to play (seconds of a 48 kHz file on one core per channel).
pub fn resample(buf: &AudioBuffer, to: u32) -> AudioBuffer {
    if buf.sample_rate == to || buf.left.is_empty() {
        return AudioBuffer::new(to, buf.left.clone(), buf.right.clone());
    }
    let k = Kernel::new(buf.sample_rate, to);
    let len = k.out_len(buf.left.len()).min(k.out_len(buf.right.len()));
    let (mut l, mut r) = (vec![0f32; len], vec![0f32; len]);
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 8));
    let chunk = len.div_ceil(workers).max(1);
    let mut parts: Vec<(&[f32], usize, &mut [f32])> = Vec::new();
    for (input, out) in [(&buf.left, &mut l), (&buf.right, &mut r)] {
        parts.extend(out.chunks_mut(chunk).enumerate().map(|(c, part)| (input.as_slice(), c * chunk, part)));
    }
    let queue = std::sync::Mutex::new(parts);
    let work = || loop {
        let Some((input, first, part)) = queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner).pop() else { break };
        k.run(input, first, part);
    };
    std::thread::scope(|s| {
        // this thread works too, so a failed spawn only makes it slower
        for _ in 1..workers {
            if let Err(e) = std::thread::Builder::new().name("resample".into()).spawn_scoped(s, work) {
                log::warn!("resample thread: {e}");
                break;
            }
        }
        work();
    });
    AudioBuffer::new(to, l, r)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sine_48k_to_44k() {
        let f = 1000.0;
        let x: Vec<f32> = (0..48000).map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / 48000.0).sin() as f32).collect();
        let y = resample_channel(&x, 48000, 44100);
        assert_eq!(y.len(), 44100);
        let mut err = 0f64;
        for (i, v) in y.iter().enumerate().skip(200).take(40000) {
            let e = (2.0 * std::f64::consts::PI * f * i as f64 / 44100.0).sin();
            err = err.max((*v as f64 - e).abs());
        }
        assert!(err < 2e-3, "max err {err}");
    }
}
