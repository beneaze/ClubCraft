//! Shared spectral front end: a 1024-point / 256-hop STFT reduced to mel band magnitudes, from
//! which onset envelopes, band energies and per-bar features are derived.

use crate::dsp::{self, MelBank};

pub const N_FFT: usize = 1024;
pub const HOP: usize = 256;
pub const N_MELS: usize = 40;

/// Mel-band magnitudes, `n_frames x N_MELS`, frame `i` centred at `i * HOP / sr` seconds.
pub struct Spectral {
    pub fps: f64,
    pub n_frames: usize,
    pub mel: Vec<f32>,
    pub centers: Vec<f32>,
}

impl Spectral {
    pub fn compute(x: &[f32], sr: f64) -> Spectral {
        let n_fft = ((N_FFT as f64 * sr / 44100.0).round() as usize).next_power_of_two().max(256);
        let hop = HOP * n_fft / N_FFT;
        let bank = MelBank::new(n_fft, sr, N_MELS, 30.0, 16000.0);
        let frames: Vec<[f32; N_MELS]> = dsp::stft_map(x, n_fft, hop, |_, spec| {
            let mut m = [0.0f32; N_MELS];
            bank.apply(spec, &mut m);
            m
        });
        let n_frames = frames.len();
        let mut mel = Vec::with_capacity(n_frames * N_MELS);
        for f in &frames {
            mel.extend_from_slice(f);
        }
        Spectral { fps: sr / hop as f64, n_frames, mel, centers: bank.centers.clone() }
    }

    #[inline]
    pub fn frame(&self, i: usize) -> &[f32] {
        &self.mel[i * N_MELS..(i + 1) * N_MELS]
    }

    /// Indices of mel bands whose centre lies in `lo..hi` Hz.
    pub fn bands(&self, lo: f32, hi: f32) -> std::ops::Range<usize> {
        let a = self.centers.iter().position(|c| *c >= lo).unwrap_or(N_MELS);
        let b = self.centers.iter().position(|c| *c >= hi).unwrap_or(N_MELS);
        a..b.max(a)
    }

    /// Log-compressed band magnitudes.
    pub fn log_mel(&self) -> Vec<f32> {
        self.mel.iter().map(|m| (1.0 + 1000.0 * m).ln()).collect()
    }

    /// SuperFlux-style onset strength restricted to `bands`: positive log-magnitude increase
    /// against the frequency-max-filtered frame two hops earlier.
    pub fn onset(&self, logmel: &[f32], bands: std::ops::Range<usize>) -> Vec<f32> {
        let n = self.n_frames;
        let lag = 2;
        let mut out = vec![0.0f32; n];
        for i in lag..n {
            let cur = &logmel[i * N_MELS..(i + 1) * N_MELS];
            let prev = &logmel[(i - lag) * N_MELS..(i - lag + 1) * N_MELS];
            let mut s = 0.0;
            for b in bands.clone() {
                let lo = b.saturating_sub(1);
                let hi = (b + 1).min(N_MELS - 1);
                let mut p = prev[lo];
                for v in &prev[lo..=hi] {
                    p = p.max(*v);
                }
                s += (cur[b] - p).max(0.0);
            }
            out[i] = s;
        }
        out
    }
}
