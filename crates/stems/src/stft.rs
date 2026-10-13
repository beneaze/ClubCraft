//! Torch-compatible STFT / iSTFT and the HTDemucs spectral wrappers (`_spec` / `_ispec`).
//!
//! `torch.stft(n_fft, hop, window=hann_window(n_fft) /*periodic*/, center=True, pad_mode="reflect",
//! normalized=True)` scales by `1/sqrt(n_fft)`. HTDemucs additionally reflect-pads the signal by
//! `3 * hop / 2` on both sides (plus up to a multiple of `hop` on the right), drops the Nyquist bin
//! and keeps frames `2 .. 2 + ceil(len / hop)`. With `hop = n_fft / 4` the kept frames never touch
//! the `center=True` padding, so we index the HTDemucs-padded signal directly.

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub struct Spectro {
    pub nfft: usize,
    pub hop: usize,
    window: Vec<f32>,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
}

impl Spectro {
    pub fn new(nfft: usize, hop: usize) -> Self {
        assert_eq!(hop * 4, nfft, "HTDemucs requires hop == nfft / 4");
        let mut planner = RealFftPlanner::<f32>::new();
        // Periodic Hann window, computed in f64 like torch does internally then rounded.
        let window = (0..nfft)
            .map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / nfft as f64).cos()) as f32)
            .collect();
        Spectro { nfft, hop, window, r2c: planner.plan_fft_forward(nfft), c2r: planner.plan_fft_inverse(nfft) }
    }

    /// Number of frequency bins kept by HTDemucs (`nfft / 2`, Nyquist dropped).
    pub fn freqs(&self) -> usize {
        self.nfft / 2
    }

    /// Number of frames HTDemucs produces for `len` samples: `ceil(len / hop)`.
    pub fn frames(&self, len: usize) -> usize {
        len.div_ceil(self.hop)
    }

    /// HTDemucs `_spec` + `_magnitude` (complex-as-channels) for one channel.
    /// Writes `re[f * T + t]` and `im[f * T + t]` for `f < nfft/2`, `t < T = frames(len)`.
    pub fn spec_channel(&self, x: &[f32], re: &mut [f32], im: &mut [f32]) {
        let len = x.len();
        let t_frames = self.frames(len);
        let nf = self.freqs();
        assert!(re.len() >= nf * t_frames && im.len() >= nf * t_frames);
        let pad = self.hop / 2 * 3;
        let padded = pad1d_reflect(x, pad, pad + t_frames * self.hop - len);
        let scale = 1.0 / (self.nfft as f32).sqrt();
        let mut buf = self.r2c.make_input_vec();
        let mut spec = self.r2c.make_output_vec();
        let mut scratch = self.r2c.make_scratch_vec();
        for t in 0..t_frames {
            let frame = &padded[t * self.hop..t * self.hop + self.nfft];
            for ((b, &s), &w) in buf.iter_mut().zip(frame).zip(&self.window) {
                *b = s * w;
            }
            self.r2c.process_with_scratch(&mut buf, &mut spec, &mut scratch).expect("fft");
            for f in 0..nf {
                re[f * t_frames + t] = spec[f].re * scale;
                im[f * t_frames + t] = spec[f].im * scale;
            }
        }
    }

    /// HTDemucs `_spec` for planar channels into a complex-as-channels buffer
    /// `[C * 2][nfft/2][T]` (ch0 re, ch0 im, ch1 re, ch1 im, ...).
    pub fn spec(&self, chans: &[&[f32]], out: &mut [f32]) {
        let plane = self.freqs() * self.frames(chans[0].len());
        assert_eq!(out.len(), plane * 2 * chans.len());
        for (c, x) in chans.iter().enumerate() {
            let (re, rest) = out[c * 2 * plane..].split_at_mut(plane);
            self.spec_channel(x, re, &mut rest[..plane]);
        }
    }

    /// HTDemucs `_ispec` for one channel: `re` / `im` are `[nfft/2][T]` planes, `out.len()` is the
    /// target length (`T == frames(out.len())`). Overwrites `out`.
    pub fn ispec_channel(&self, re: &[f32], im: &[f32], out: &mut [f32]) {
        let length = out.len();
        let t_frames = self.frames(length);
        let nf = self.freqs();
        let (nfft, hop) = (self.nfft, self.hop);
        assert!(re.len() >= nf * t_frames && im.len() >= nf * t_frames);
        let pad = (hop / 2 * 3) as isize;
        out.iter_mut().for_each(|v| *v = 0.0);
        let mut spec = self.c2r.make_input_vec();
        let mut frame = self.c2r.make_output_vec();
        let mut scratch = self.c2r.make_scratch_vec();
        let scale = 1.0 / (nfft as f32).sqrt();
        for t in 0..t_frames {
            for f in 0..nf {
                spec[f] = Complex32::new(re[f * t_frames + t], im[f * t_frames + t]);
            }
            // C2R ignores the imaginary part of DC and Nyquist (like torch / pocketfft).
            spec[0].im = 0.0;
            spec[nf] = Complex32::new(0.0, 0.0);
            self.c2r.process_with_scratch(&mut spec, &mut frame, &mut scratch).expect("ifft");
            let start = t as isize * hop as isize - pad;
            let n0 = (-start).max(0) as usize;
            let n1 = ((length as isize - start).min(nfft as isize)).max(0) as usize;
            for n in n0..n1 {
                out[(start + n as isize) as usize] += frame[n] * scale * self.window[n];
            }
        }
        // Window-square envelope over all frames of the padded istft (2 zero frames each side).
        let mut env = vec![0f32; length];
        for t in -2..(t_frames as isize + 2) {
            let start = t * hop as isize - pad;
            let n0 = (-start).max(0) as usize;
            let n1 = ((length as isize - start).min(nfft as isize)).max(0) as usize;
            for n in n0..n1 {
                env[(start + n as isize) as usize] += self.window[n] * self.window[n];
            }
        }
        for (o, e) in out.iter_mut().zip(&env) {
            *o /= *e;
        }
    }
}

/// `demucs.hdemucs.pad1d(x, (left, right), mode="reflect")`: reflect padding that first zero-pads
/// inputs that are too short to reflect.
pub fn pad1d_reflect(x: &[f32], left: usize, right: usize) -> Vec<f32> {
    let length = x.len();
    let max_pad = left.max(right);
    let (mut src, mut l, mut r) = (x.to_vec(), left, right);
    if length <= max_pad {
        let extra = max_pad - length + 1;
        let extra_r = right.min(extra);
        let extra_l = extra - extra_r;
        l -= extra_l;
        r -= extra_r;
        let mut z = vec![0f32; extra_l];
        z.extend_from_slice(x);
        z.resize(z.len() + extra_r, 0.0);
        src = z;
    }
    let n = src.len() as isize;
    let mut out = Vec::with_capacity(src.len() + l + r);
    for i in -(l as isize)..(n + r as isize) {
        let j = if i < 0 {
            -i
        } else if i >= n {
            2 * (n - 1) - i
        } else {
            i
        };
        out.push(src[j as usize]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let s = Spectro::new(4096, 1024);
        let len = 50_000;
        let x: Vec<f32> = (0..len).map(|i| ((i as f32) * 0.01).sin() * 0.5 + ((i * 7919) % 101) as f32 * 1e-3).collect();
        let t = s.frames(len);
        let mut re = vec![0f32; 2048 * t];
        let mut im = vec![0f32; 2048 * t];
        s.spec_channel(&x, &mut re, &mut im);
        let mut y = vec![0f32; len];
        s.ispec_channel(&re, &im, &mut y);
        // Dropping the Nyquist bin loses a tiny bit; the rest must reconstruct.
        let err = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(err < 1e-3, "roundtrip err {err}");
    }

    #[test]
    fn reflect() {
        assert_eq!(pad1d_reflect(&[1., 2., 3.], 2, 2), vec![3., 2., 1., 2., 3., 2., 1.]);
    }
}
