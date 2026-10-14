//! COCOLA (Ciranni et al., ICASSP 2025, MIT): a learned score of how well two clips fit together
//! harmonically and rhythmically, used to re-rank mashup candidates.
//!
//! The model (`models/cocola.onnx`, exported by `tools/model-export/export_cocola.py`) embeds a
//! 5 s, 16 kHz clip; the score of a pair is `x · W y` with the bilinear matrix in
//! `models/cocola_w.bin`. The front end is a port of the original librosa pipeline: centred STFT
//! (n_fft 1024, Hann 400, hop 160) → median-filter HPSS (kernel 31, soft masks, power 2) →
//! Slaney mel (64 bands, 60 Hz - 7.8 kHz) → dB relative to the maximum, 80 dB floor, giving a
//! `[2, 64, 501]` (harmonic, percussive) input. `tests/cocola_ref.rs` checks it against librosa.

use realfft::RealFftPlanner;

pub const SR: u32 = 16_000;
pub const CLIP: usize = 5 * SR as usize;
const N_FFT: usize = 1024;
const WIN: usize = 400;
const HOP: usize = 160;
const N_BINS: usize = N_FFT / 2 + 1;
pub const FRAMES: usize = CLIP / HOP + 1;
pub const N_MELS: usize = 64;
const KERNEL: usize = 31;
pub const DIM: usize = 512;

/// Magnitude STFT `[N_BINS][FRAMES]` of a clip (zero padded / cut to 5 s).
fn stft_mag(x: &[f32]) -> Vec<Vec<f32>> {
    let plan = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let off = (N_FFT - WIN) / 2;
    let mut win = vec![0f32; N_FFT];
    for (j, w) in win.iter_mut().enumerate().skip(off).take(WIN) {
        *w = (0.5 - 0.5 * (2.0 * std::f64::consts::PI * (j - off) as f64 / WIN as f64).cos()) as f32;
    }
    let (mut inp, mut out, mut scratch) = (plan.make_input_vec(), plan.make_output_vec(), plan.make_scratch_vec());
    let mut mag = vec![vec![0f32; FRAMES]; N_BINS];
    let half = (N_FFT / 2) as isize;
    for t in 0..FRAMES {
        for (j, v) in inp.iter_mut().enumerate() {
            let i = (t * HOP) as isize + j as isize - half;
            *v = if i >= 0 && (i as usize) < CLIP && (i as usize) < x.len() { x[i as usize] * win[j] } else { 0.0 };
        }
        let _ = plan.process_with_scratch(&mut inp, &mut out, &mut scratch);
        for (k, c) in out.iter().enumerate() {
            mag[k][t] = c.norm();
        }
    }
    mag
}

/// Median of a window with scipy's `reflect` boundary (edge sample repeated: d c b a | a b c d).
fn median_filter_1d(x: &[f32], out: &mut [f32], buf: &mut Vec<f32>) {
    let n = x.len() as isize;
    let r = (KERNEL / 2) as isize;
    let refl = |i: isize| -> usize {
        let mut i = i;
        loop {
            if i < 0 {
                i = -i - 1;
            } else if i >= n {
                i = 2 * n - i - 1;
            } else {
                return i as usize;
            }
        }
    };
    for (c, o) in out.iter_mut().enumerate() {
        buf.clear();
        buf.extend((c as isize - r..=c as isize + r).map(|i| x[refl(i)]));
        let (_, m, _) = buf.select_nth_unstable_by(KERNEL / 2, |a, b| a.total_cmp(b));
        *o = *m;
    }
}

fn hz_to_mel(f: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let logstep = 6.4f64.ln() / 27.0;
    if f >= 1000.0 {
        1000.0 / f_sp + (f / 1000.0).ln() / logstep
    } else {
        f / f_sp
    }
}

fn mel_to_hz(m: f64) -> f64 {
    let f_sp = 200.0 / 3.0;
    let logstep = 6.4f64.ln() / 27.0;
    let min_log_mel = 1000.0 / f_sp;
    if m >= min_log_mel {
        1000.0 * (logstep * (m - min_log_mel)).exp()
    } else {
        f_sp * m
    }
}

/// librosa.filters.mel(sr=16000, n_fft=1024, n_mels=64, fmin=60, fmax=7800, slaney norm).
fn mel_basis() -> Vec<Vec<f32>> {
    let (lo, hi) = (hz_to_mel(60.0), hz_to_mel(7800.0));
    let mel_f: Vec<f64> = (0..N_MELS + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (N_MELS + 1) as f64)).collect();
    let fft_f: Vec<f64> = (0..N_BINS).map(|k| k as f64 * SR as f64 / N_FFT as f64).collect();
    (0..N_MELS)
        .map(|i| {
            let enorm = 2.0 / (mel_f[i + 2] - mel_f[i]);
            fft_f
                .iter()
                .map(|&f| {
                    let lower = (f - mel_f[i]) / (mel_f[i + 1] - mel_f[i]);
                    let upper = (mel_f[i + 2] - f) / (mel_f[i + 2] - mel_f[i + 1]);
                    (lower.min(upper).max(0.0) as f32) * enorm as f32
                })
                .collect()
        })
        .collect()
}

/// COCOLA input features `[2 * 64 * 501]` (harmonic then percussive) of a 16 kHz mono clip.
pub fn features(clip: &[f32]) -> Vec<f32> {
    let mag = stft_mag(clip);
    // HPSS: harmonic = median over time, percussive = median over frequency
    let mut harm = vec![vec![0f32; FRAMES]; N_BINS];
    let mut perc = vec![vec![0f32; FRAMES]; N_BINS];
    let mut buf = Vec::with_capacity(KERNEL);
    for k in 0..N_BINS {
        median_filter_1d(&mag[k], &mut harm[k], &mut buf);
    }
    let mut col = vec![0f32; N_BINS];
    let mut colo = vec![0f32; N_BINS];
    for t in 0..FRAMES {
        for k in 0..N_BINS {
            col[k] = mag[k][t];
        }
        median_filter_1d(&col, &mut colo, &mut buf);
        for k in 0..N_BINS {
            perc[k][t] = colo[k];
        }
    }
    let basis = mel_basis();
    let mut out = Vec::with_capacity(2 * N_MELS * FRAMES);
    for which in 0..2 {
        // soft mask (power 2, zeros split 50/50) applied to the magnitude, then power
        let mut mel = vec![0f32; N_MELS * FRAMES];
        let mut pow = vec![0f32; N_BINS];
        for t in 0..FRAMES {
            for k in 0..N_BINS {
                let (h, p) = (harm[k][t], perc[k][t]);
                let (x, r) = if which == 0 { (h, p) } else { (p, h) };
                let z = x.max(r);
                let m = if z < f32::MIN_POSITIVE {
                    0.5
                } else {
                    let (a, b) = ((x / z).powi(2), (r / z).powi(2));
                    a / (a + b)
                };
                let v = mag[k][t] * m;
                pow[k] = v * v;
            }
            for (i, row) in basis.iter().enumerate() {
                mel[i * FRAMES + t] = row.iter().zip(&pow).map(|(w, p)| w * p).sum();
            }
        }
        // power_to_db(ref = max, amin 1e-10, top_db 80)
        let reference = mel.iter().cloned().fold(0.0f32, f32::max).max(1e-10);
        let ref_db = 10.0 * reference.log10();
        let mut db: Vec<f32> = mel.iter().map(|&v| 10.0 * v.max(1e-10).log10() - ref_db).collect();
        let top = db.iter().cloned().fold(f32::MIN, f32::max);
        for v in &mut db {
            *v = v.max(top - 80.0);
        }
        out.extend(db);
    }
    out
}

/// 5 s clip at 16 kHz from 44.1 kHz mono audio starting at `start` (zero padded).
pub fn clip_from(x: &[f32], start: usize) -> Vec<f32> {
    let n = (CLIP as f64 * clubcraft_core::SAMPLE_RATE as f64 / SR as f64).ceil() as usize + 64;
    let end = (start + n).min(x.len());
    let seg = if start < end { &x[start..end] } else { &[][..] };
    let mut c = clubcraft_audio::resample::resample_channel(seg, clubcraft_core::SAMPLE_RATE, SR);
    c.resize(CLIP, 0.0);
    c
}

/// Raw COCOLA score → 0..1. On separated house / pop stems a vocal over its own instrumental
/// scores ~55.6 and random pairings ~49.5 (σ ≈ 6.4).
pub fn normalized(raw: f32) -> f32 {
    1.0 / (1.0 + (-(raw - 52.0) / 3.5).exp())
}

#[cfg(feature = "allinone")]
pub struct Cocola {
    session: ort::session::Session,
    w: Vec<f32>,
}

#[cfg(feature = "allinone")]
impl Cocola {
    pub fn available() -> bool {
        let d = clubcraft_core::paths::models_dir();
        d.join("cocola.onnx").exists() && d.join("cocola_w.bin").exists()
    }

    pub fn load_default() -> anyhow::Result<Cocola> {
        use anyhow::Context;
        let d = clubcraft_core::paths::models_dir();
        let wb = std::fs::read(d.join("cocola_w.bin")).context("read cocola_w.bin")?;
        anyhow::ensure!(wb.len() == DIM * DIM * 4, "cocola_w.bin has the wrong size");
        let w = wb.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        let session = ort::session::Session::builder()?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
            .with_intra_threads(4)?
            .with_memory_pattern(false)?
            .commit_from_file(d.join("cocola.onnx"))
            .context("load cocola.onnx")?;
        Ok(Cocola { session, w })
    }

    /// Embeddings of feature tensors (see [`features`]); `harmonic_only` zeroes the percussive
    /// channel (COCOLA's harmonic score).
    pub fn embed(&self, feats: &[Vec<f32>], harmonic_only: bool) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(feats.len());
        for chunk in feats.chunks(8) {
            let mut data = Vec::with_capacity(chunk.len() * 2 * N_MELS * FRAMES);
            for f in chunk {
                data.extend_from_slice(&f[..N_MELS * FRAMES]);
                if harmonic_only {
                    data.extend(std::iter::repeat_n(0.0, N_MELS * FRAMES));
                } else {
                    data.extend_from_slice(&f[N_MELS * FRAMES..]);
                }
            }
            let t = ort::value::Tensor::from_array((vec![chunk.len(), 2, N_MELS, FRAMES], data))?;
            let outputs = self.session.run(ort::inputs!["features" => t]?)?;
            let (_, e) = outputs["embedding"].try_extract_raw_tensor::<f32>()?;
            out.extend(e.chunks_exact(DIM).map(|c| c.to_vec()));
        }
        Ok(out)
    }

    /// Raw score of `x` (e.g. vocals) with `y` (e.g. instrumental): `x · W y`.
    pub fn score(&self, x: &[f32], y: &[f32]) -> f32 {
        (0..DIM).map(|i| x[i] * self.w[i * DIM..(i + 1) * DIM].iter().zip(y).map(|(a, b)| a * b).sum::<f32>()).sum()
    }
}
