//! EBU R128 / ITU-R BS.1770-4 integrated loudness.
//!
//! K-weighting (high shelf + RLB high pass, coefficients derived for any sample rate),
//! 400 ms blocks with 75 % overlap, absolute gate at −70 LUFS and relative gate at −10 LU.

use crate::dsp::Biquad;
use rayon::prelude::*;

fn k_weighting(sr: f64) -> (Biquad, Biquad) {
    use std::f64::consts::PI;
    // Stage 1: high shelf.
    let (g, f0, q) = (3.999843853973347, 1681.974450955533, 0.7071752369554196);
    let k = (PI * f0 / sr).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad::raw(
        (vh + vb * k / q + k * k) / a0,
        2.0 * (k * k - vh) / a0,
        (vh - vb * k / q + k * k) / a0,
        2.0 * (k * k - 1.0) / a0,
        (1.0 - k / q + k * k) / a0,
    );
    // Stage 2: RLB high pass.
    let (f0, q) = (38.13547087602444, 0.5003270373238773);
    let k = (PI * f0 / sr).tan();
    let a0 = 1.0 + k / q + k * k;
    let hp = Biquad::raw(
        1.0,
        -2.0,
        1.0,
        2.0 * (k * k - 1.0) / a0,
        (1.0 - k / q + k * k) / a0,
    );
    (shelf, hp)
}

/// Sum of K-weighted squares per 100 ms step.
fn step_energies(x: &[f32], sr: f64, step: usize) -> Vec<f64> {
    let (mut a, mut b) = k_weighting(sr);
    let mut out = Vec::with_capacity(x.len() / step + 1);
    for chunk in x.chunks(step) {
        let mut s = 0.0;
        for v in chunk {
            let y = b.tick(a.tick(*v as f64));
            s += y * y;
        }
        out.push(s);
    }
    out
}

/// Integrated loudness (LUFS) of a stereo signal. `None` for silence / too short input.
pub fn integrated_lufs(left: &[f32], right: &[f32], sample_rate: u32) -> Option<f32> {
    let sr = sample_rate as f64;
    let step = (sr * 0.1).round() as usize;
    let chans: Vec<Vec<f64>> = [left, right]
        .par_iter()
        .map(|c| step_energies(c, sr, step))
        .collect();
    let n = chans[0].len().min(chans[1].len());
    if n < 4 {
        return None;
    }
    let block_len = (4 * step) as f64;
    // Mean square (summed over channels, weights 1.0 for L/R) of each 400 ms block.
    let blocks: Vec<f64> = (0..=n - 4)
        .map(|i| {
            (0..4)
                .map(|j| chans[0][i + j] + chans[1][i + j])
                .sum::<f64>()
                / block_len
        })
        .collect();
    let lk = |z: f64| -0.691 + 10.0 * z.max(1e-20).log10();
    let abs: Vec<f64> = blocks.iter().copied().filter(|z| lk(*z) > -70.0).collect();
    if abs.is_empty() {
        return None;
    }
    let rel_gate = lk(abs.iter().sum::<f64>() / abs.len() as f64) - 10.0;
    let gated: Vec<f64> = abs.into_iter().filter(|z| lk(*z) > rel_gate).collect();
    if gated.is_empty() {
        return None;
    }
    Some(lk(gated.iter().sum::<f64>() / gated.len() as f64) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sine_1k_minus_20() {
        // A 1 kHz sine at -20 dBFS peak on both channels is ~ -20 LUFS (+3 dB for two channels,
        // -3 dB sine RMS, ~0 dB K-weighting at 1 kHz) => about -20.0.
        let sr = 48000;
        let a = 10f32.powf(-20.0 / 20.0);
        let x: Vec<f32> = (0..sr * 5)
            .map(|i| a * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin())
            .collect();
        let l = integrated_lufs(&x, &x, sr as u32).unwrap();
        assert!((l + 20.0).abs() < 0.2, "{l}");
    }
}
