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

/// Resample one channel from `from` Hz to `to` Hz.
pub fn resample_channel(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    const HALF: i64 = 64;
    const PHASES: usize = 512;
    let ratio = to as f64 / from as f64;
    let cutoff = ratio.min(1.0) * 0.97; // relative to input Nyquist
    let beta = 9.0;
    let i0b = bessel_i0(beta);
    // Precompute the kernel at PHASES sub-sample offsets.
    let taps = (2 * HALF) as usize;
    let mut table = vec![0f32; (PHASES + 1) * taps];
    for p in 0..=PHASES {
        let frac = p as f64 / PHASES as f64;
        for k in 0..taps {
            let x = (k as i64 - HALF + 1) as f64 - frac;
            let s = if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * x * cutoff).sin() / (std::f64::consts::PI * x * cutoff) };
            let w = {
                let t = x / HALF as f64;
                if t.abs() >= 1.0 { 0.0 } else { bessel_i0(beta * (1.0 - t * t).sqrt()) / i0b }
            };
            table[p * taps + k] = (s * w * cutoff) as f32;
        }
    }
    let out_len = ((input.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    let n = input.len() as i64;
    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let base = pos.floor() as i64;
        let frac = pos - base as f64;
        let pf = frac * PHASES as f64;
        let p0 = pf.floor() as usize;
        let t = (pf - p0 as f64) as f32;
        let k0 = &table[p0 * taps..(p0 + 1) * taps];
        let k1 = &table[(p0 + 1) * taps..(p0 + 2) * taps];
        let mut acc = 0f32;
        let start = base - HALF + 1;
        for k in 0..taps {
            let idx = start + k as i64;
            if idx >= 0 && idx < n {
                let c = k0[k] + (k1[k] - k0[k]) * t;
                acc += input[idx as usize] * c;
            }
        }
        out.push(acc);
    }
    out
}

pub fn resample(buf: &AudioBuffer, to: u32) -> AudioBuffer {
    let (l, r) = std::thread::scope(|s| {
        let a = s.spawn(|| resample_channel(&buf.left, buf.sample_rate, to));
        let b = resample_channel(&buf.right, buf.sample_rate, to);
        (a.join().unwrap_or_else(|_| vec![0.0; b.len()]), b)
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
