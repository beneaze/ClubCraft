//! Waveform summary for the waveform views.
//!
//! 256-sample bins (172.27 bins/s at 44.1 kHz). Per bin: full-mix peak plus low / mid / high
//! band peaks split by 4th-order Linkwitz-Riley crossovers at 200 Hz and 2.5 kHz, and per-stem
//! peaks when stems are available. Values are `255 * sqrt(peak)` (peak in 0..1 full scale),
//! a perceptual, loudness-like scale that keeps quiet detail visible.

use crate::dsp;
use crate::StemsRef;
use clubcraft_audio::AudioBuffer;
use clubcraft_core::Waveform;
use rayon::prelude::*;

/// Samples per waveform bin.
pub const BIN: usize = 256;
pub const LOW_HZ: f64 = 200.0;
pub const HIGH_HZ: f64 = 2500.0;

#[inline]
fn scale(p: f32) -> u8 {
    (p.clamp(0.0, 1.0).sqrt() * 255.0).round() as u8
}

fn peaks(x: &[f32], bins: usize) -> Vec<u8> {
    (0..bins)
        .map(|i| {
            let a = i * BIN;
            let b = ((i + 1) * BIN).min(x.len());
            if a >= b {
                return 0;
            }
            scale(x[a..b].iter().fold(0.0f32, |m, v| m.max(v.abs())))
        })
        .collect()
}

fn stereo_peaks(l: &[f32], r: &[f32], bins: usize) -> Vec<u8> {
    (0..bins)
        .map(|i| {
            let a = i * BIN;
            let b = ((i + 1) * BIN).min(l.len()).min(r.len());
            if a >= b {
                return 0;
            }
            let mut m = 0.0f32;
            for j in a..b {
                m = m.max(l[j].abs()).max(r[j].abs());
            }
            scale(m)
        })
        .collect()
}

/// Band-filtered bin peaks, computed in parallel chunks (each chunk's IIR filters are warmed
/// up on the preceding 8192 samples, far longer than their impulse response at 200 Hz).
fn band_peaks(x: &[f32], sr: f64, band: u8, bins: usize) -> Vec<u8> {
    const CHUNK_BINS: usize = 1024;
    const WARM: usize = 8192;
    let chunks = bins.div_ceil(CHUNK_BINS);
    let parts: Vec<Vec<u8>> = (0..chunks)
        .into_par_iter()
        .map(|c| {
            let b0 = c * CHUNK_BINS;
            let b1 = ((c + 1) * CHUNK_BINS).min(bins);
            let s0 = b0 * BIN;
            let s1 = (b1 * BIN).min(x.len());
            let w0 = s0.saturating_sub(WARM);
            let seg = &x[w0..s1.max(w0)];
            let y = match band {
                0 => dsp::lr4(seg, LOW_HZ, sr, false),
                1 => dsp::lr4(&dsp::lr4(seg, LOW_HZ, sr, true), HIGH_HZ, sr, false),
                _ => dsp::lr4(seg, HIGH_HZ, sr, true),
            };
            let y = &y[s0 - w0..];
            peaks(y, b1 - b0)
        })
        .collect();
    parts.concat()
}

/// Compute the waveform summary of a track (and its stems, if given).
pub fn waveform(audio: &AudioBuffer, stems: Option<StemsRef>) -> Waveform {
    let sr = audio.sample_rate as f64;
    let n = audio.frames();
    let bins = n.div_ceil(BIN);
    let mono = audio.mono();
    let peak = stereo_peaks(&audio.left, &audio.right, bins);
    let low = band_peaks(&mono, sr, 0, bins);
    let mid = band_peaks(&mono, sr, 1, bins);
    let high = band_peaks(&mono, sr, 2, bins);
    let stems = stems.map(|s| {
        let v: Vec<Vec<u8>> = s.stems.par_iter().map(|(l, r)| stereo_peaks(l, r, bins)).collect();
        let mut it = v.into_iter();
        [it.next().unwrap(), it.next().unwrap(), it.next().unwrap(), it.next().unwrap()]
    });
    Waveform { rate: sr / BIN as f64, peak, low, mid, high, stems }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bands_split() {
        let sr = 44100;
        let n = sr as usize * 3;
        let tone = |f: f32| -> Vec<f32> { (0..n).map(|i| 0.5 * (2.0 * std::f32::consts::PI * f * i as f32 / sr as f32).sin()).collect() };
        let lo = AudioBuffer::new(sr, tone(60.0), tone(60.0));
        let w = waveform(&lo, None);
        assert!((w.rate - 172.265625).abs() < 1e-6);
        let mid_bin = w.len() / 2;
        assert!(w.low[mid_bin] > 150, "{}", w.low[mid_bin]);
        assert!(w.high[mid_bin] < 20, "{}", w.high[mid_bin]);
        let hi = AudioBuffer::new(sr, tone(8000.0), tone(8000.0));
        let w = waveform(&hi, None);
        assert!(w.high[mid_bin] > 150 && w.low[mid_bin] < 20);
    }
}
