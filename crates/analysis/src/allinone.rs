//! All-In-One music structure model (Kim & Choi, WASPAA 2023; github.com/mir-aidj/all-in-one,
//! MIT licence) — Rust front end, ONNX runtime and post-processing.
//!
//! The network (8-fold Harmonix ensemble, exported by `tools/model-export/export_allin1.py`
//! without the NATTEN custom ops) takes, per stem (bass, drums, other, vocals), a madmom-style
//! log-filtered spectrogram: 2048-sample frames at 100 fps (hop 441, centred, symmetric Hann),
//! STFT magnitude → 81 triangular log-spaced bands (12 per octave, 30 Hz..17 kHz, normalised)
//! → `log10(1 + x)`. It outputs per frame: beat, downbeat and section-boundary probabilities and
//! a distribution over 10 functional labels (start, end, intro, outro, break, bridge, inst, solo,
//! verse, chorus).
//!
//! ClubCraft keeps its DSP beat grid (more precise for tempo / phase, see [`crate::beatgrid`])
//! and uses the model for **section boundaries** (peaks snapped to bars / 4-bar lines) with
//! the activity-based labelling refined by the model's functional labels
//! ([`crate::structure::segment_with_model`]). The model needs stems and at least
//! [`MIN_FRAMES`] frames (102.4 s); otherwise, or without `models/allin1.onnx` (or with the
//! `allinone` cargo feature off), the rule-based structure is used.
//!
//! Against Mixed In Key's cue points on 31 tracks (Rust `htdemucs` stems), boundaries hit
//! 85 % of MIK cues within one bar (rules: 72 %), and the final cue set 73 % (rules: 67 %).
//!
//! Cost (ONNX Runtime CPU, 8-fold ensemble): roughly 15–30 s and ~2 GB peak memory for a
//! 5–6 minute track on an M1 Pro under load, so run it in the background after separation.
//! Export: `tools/model-export/export_allin1.py` (NATTEN replaced by exact gather/einsum code;
//! ONNX output equals the PyTorch reference to 4e-7).

use crate::StemsRef;
use rayon::prelude::*;
use realfft::RealFftPlanner;

pub const FPS: f64 = 100.0;
pub const HOP: usize = 441;
pub const N_FFT: usize = 2048;
pub const N_BANDS: usize = 81;
/// Shortest input the exported graph accepts (its longest attention window, 102.4 s).
pub const MIN_FRAMES: usize = 10240;
/// Functional labels in output order.
pub const LABELS: [&str; 10] = [
    "start", "end", "intro", "outro", "break", "bridge", "inst", "solo", "verse", "chorus",
];

/// Sparse triangular log filterbank: per band (first bin, weights).
fn filterbank() -> Vec<(usize, Vec<f32>)> {
    let sr = 44100.0f64;
    let nb = N_FFT / 2;
    let bin_f: Vec<f64> = (0..nb).map(|i| i as f64 * sr / N_FFT as f64).collect();
    let (bands, fmin, fmax, fref) = (12.0f64, 30.0f64, 17000.0f64, 440.0f64);
    let left = ((fmin / fref).log2() * bands).floor() as i64;
    let right = ((fmax / fref).log2() * bands).ceil() as i64;
    let freqs: Vec<f64> = (left..right)
        .map(|k| fref * 2f64.powf(k as f64 / bands))
        .filter(|f| *f >= fmin && *f <= fmax)
        .collect();
    let mut bins: Vec<usize> = freqs
        .iter()
        .map(|&f| {
            let mut i = bin_f.partition_point(|b| *b < f).clamp(1, nb - 1);
            if f - bin_f[i - 1] < bin_f[i] - f {
                i -= 1;
            }
            i
        })
        .collect();
    bins.dedup();
    let mut out = Vec::new();
    for w in bins.windows(3) {
        let (start, mut center, mut stop) = (w[0], w[1], w[2]);
        if stop - start < 2 {
            center = start;
            stop = start + 1;
        }
        let mut data = vec![0.0f64; stop - start];
        let rise = center - start;
        for (j, d) in data.iter_mut().take(rise).enumerate() {
            *d = j as f64 / rise as f64;
        }
        let fall = stop - center;
        for j in 0..fall {
            data[rise + j] = 1.0 - j as f64 / fall as f64;
        }
        let s: f64 = data.iter().sum();
        out.push((start, data.iter().map(|v| (v / s) as f32).collect()));
    }
    out
}

/// Log-filtered spectrogram of one mono signal: `frames x N_BANDS`.
pub fn spectrogram(x: &[f32]) -> Vec<f32> {
    let frames = x.len().div_ceil(HOP);
    let fb = filterbank();
    debug_assert_eq!(fb.len(), N_BANDS);
    let win: Vec<f32> = (0..N_FFT)
        .map(|n| {
            (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / (N_FFT - 1) as f64).cos()) as f32
        })
        .collect();
    let plan = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let half = (N_FFT / 2) as isize;
    let rows: Vec<[f32; N_BANDS]> = (0..frames)
        .into_par_iter()
        .map_init(
            || {
                (
                    plan.make_input_vec(),
                    plan.make_output_vec(),
                    plan.make_scratch_vec(),
                    vec![0.0f32; N_FFT / 2],
                )
            },
            |(inp, out, scr, mag), i| {
                let start = (i * HOP) as isize - half;
                for (j, o) in inp.iter_mut().enumerate() {
                    let k = start + j as isize;
                    *o = if k >= 0 && (k as usize) < x.len() {
                        x[k as usize] * win[j]
                    } else {
                        0.0
                    };
                }
                if plan.process_with_scratch(inp, out, scr).is_err() {
                    out.fill(Default::default());
                }
                for (m, c) in mag.iter_mut().zip(out.iter()) {
                    *m = c.norm();
                }
                let mut row = [0.0f32; N_BANDS];
                for (r, (s, w)) in row.iter_mut().zip(&fb) {
                    let v: f32 = w.iter().zip(&mag[*s..]).map(|(a, b)| a * b).sum();
                    *r = (1.0 + v).log10();
                }
                row
            },
        )
        .collect();
    let mut flat = Vec::with_capacity(frames * N_BANDS);
    for r in &rows {
        flat.extend_from_slice(r);
    }
    flat
}

/// Model input `[4, T, 81]` (bass, drums, other, vocals) from ClubCraft stems.
pub fn stem_spectrograms(stems: &StemsRef) -> (usize, Vec<f32>) {
    use clubcraft_core::StemKind::*;
    let order = [Bass, Drums, Other, Vocals];
    let specs: Vec<Vec<f32>> = order
        .par_iter()
        .map(|k| spectrogram(&stems.mono(*k)))
        .collect();
    let t = specs.iter().map(|s| s.len() / N_BANDS).min().unwrap_or(0);
    let mut out = Vec::with_capacity(4 * t * N_BANDS);
    for s in &specs {
        out.extend_from_slice(&s[..t * N_BANDS]);
    }
    (t, out)
}

/// Model output: per-frame activations.
#[derive(Debug, Clone, Default)]
pub struct Activations {
    pub beat: Vec<f32>,
    pub downbeat: Vec<f32>,
    pub section: Vec<f32>,
    /// `T x 10` label probabilities.
    pub labels: Vec<[f32; 10]>,
}

impl Activations {
    /// From the `[T, 13]` output layout (sigmoid beat, downbeat, section; softmax labels).
    pub fn from_flat(data: &[f32]) -> Activations {
        let t = data.len() / 13;
        let mut a = Activations::default();
        for r in data.as_chunks::<13>().0.iter().take(t) {
            a.beat.push(r[0]);
            a.downbeat.push(r[1]);
            a.section.push(r[2]);
            let mut l = [0.0f32; 10];
            l.copy_from_slice(&r[3..13]);
            a.labels.push(l);
        }
        a
    }

    pub fn len(&self) -> usize {
        self.section.len()
    }

    pub fn is_empty(&self) -> bool {
        self.section.is_empty()
    }
}

/// A section boundary candidate from the model: time (s) and peak strength.
#[derive(Debug, Clone, Copy)]
pub struct Boundary {
    pub time: f64,
    pub strength: f32,
}

/// All-In-One's boundary post-processing: local maxima over ±48 frames (one beat at 240 BPM x 4),
/// then peaks over ±12 s whose height exceeds the mean of the surrounding ±12 s.
pub fn boundaries(act: &Activations) -> Vec<Boundary> {
    let x = &act.section;
    let n = x.len();
    let lm = |v: &[f32], half: usize| -> Vec<f32> {
        (0..n)
            .map(|i| {
                let a = i.saturating_sub(half);
                let b = (i + half + 1).min(n);
                let m = v[a..b].iter().cloned().fold(f32::MIN, f32::max);
                if v[i] == m {
                    v[i]
                } else {
                    0.0
                }
            })
            .collect()
    };
    let p1 = lm(x, 48);
    let w = 1200usize;
    let mut pre = vec![0.0f64; n + 1];
    for i in 0..n {
        pre[i + 1] = pre[i] + p1[i] as f64;
    }
    let sum = |a: isize, b: isize| -> f64 {
        let a = a.clamp(0, n as isize) as usize;
        let b = b.clamp(0, n as isize) as usize;
        pre[b] - pre[a]
    };
    let mut out = Vec::new();
    for i in 0..n {
        let v = p1[i];
        if v <= 0.0 {
            continue;
        }
        let a = i.saturating_sub(w);
        let b = (i + w + 1).min(n);
        if p1[a..b].iter().any(|u| *u > v) {
            continue;
        }
        let ii = i as isize;
        let past = sum(ii - w as isize, ii) / w as f64;
        let fut = sum(ii + 1, ii + 1 + w as isize) / w as f64;
        let s = v as f64 - 0.5 * (past + fut);
        if s > 0.0 {
            out.push(Boundary {
                time: i as f64 / FPS,
                strength: s as f32,
            });
        }
    }
    out
}

/// Mean label distribution over `[t0, t1)` seconds.
pub fn mean_labels(act: &Activations, t0: f64, t1: f64) -> [f32; 10] {
    let a = ((t0 * FPS).round().max(0.0) as usize).min(act.len());
    let b = ((t1 * FPS).round().max(0.0) as usize).min(act.len());
    let mut m = [0.0f32; 10];
    if b <= a {
        return m;
    }
    for r in &act.labels[a..b] {
        for k in 0..10 {
            m[k] += r[k];
        }
    }
    for v in &mut m {
        *v /= (b - a) as f32;
    }
    m
}

/// Loaded ONNX model (`models/allin1.onnx`).
#[cfg(feature = "allinone")]
pub struct Model {
    session: ort::session::Session,
}

#[cfg(feature = "allinone")]
impl Model {
    /// Load from `<models dir>/allin1.onnx` (see [`clubcraft_core::paths::models_dir`]).
    pub fn load_default() -> anyhow::Result<Model> {
        Self::load(&clubcraft_core::paths::models_dir().join("allin1.onnx"))
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Model> {
        use anyhow::Context;
        if !path.exists() {
            anyhow::bail!("missing {}", path.display());
        }
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let session = ort::session::Session::builder()?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)?
            .with_intra_threads(threads.min(6))?
            .with_memory_pattern(false)?
            .commit_from_file(path)
            .with_context(|| format!("load {}", path.display()))?;
        Ok(Model { session })
    }

    /// Run on a `[4, T, 81]` spectrogram.
    pub fn run(&self, frames: usize, spec: &[f32]) -> anyhow::Result<Activations> {
        let t = ort::value::Tensor::from_array((vec![1usize, 4, frames, N_BANDS], spec))?;
        let outputs = self.session.run(ort::inputs!["spec" => t]?)?;
        let (_, data) = outputs["act"].try_extract_raw_tensor::<f32>()?;
        Ok(Activations::from_flat(data))
    }

    /// Spectrogram + inference for a track's stems.
    pub fn activations(&self, stems: &StemsRef) -> anyhow::Result<Activations> {
        let (t, spec) = stem_spectrograms(stems);
        if t < MIN_FRAMES {
            anyhow::bail!("track too short for the structure model ({t} < {MIN_FRAMES} frames)");
        }
        self.run(t, &spec)
    }
}

/// Activations from the process-wide model (loaded once from `models/allin1.onnx`); `None`
/// when the feature is disabled, the model is missing or the track is too short.
pub fn shared_activations(stems: &StemsRef) -> Option<Activations> {
    #[cfg(feature = "allinone")]
    {
        if stems.frames().div_ceil(HOP) < MIN_FRAMES {
            return None;
        }
        // Loaded per call (the model is small) and dropped afterwards: ONNX Runtime's memory
        // arena never shrinks, so a cached session would keep its peak allocation forever.
        static MISSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if MISSING.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        let model = match Model::load_default() {
            Ok(m) => m,
            Err(e) => {
                log::info!("All-In-One structure model unavailable ({e:#}); using rule-based structure");
                MISSING.store(true, std::sync::atomic::Ordering::Relaxed);
                return None;
            }
        };
        let m = &model;
        let t = std::time::Instant::now();
        match m.activations(stems) {
            Ok(a) => {
                log::debug!("All-In-One model {:.2?}", t.elapsed());
                Some(a)
            }
            Err(e) => {
                log::warn!("All-In-One model failed: {e:#}");
                None
            }
        }
    }
    #[cfg(not(feature = "allinone"))]
    {
        let _ = stems;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filterbank_matches_madmom_shape() {
        let fb = filterbank();
        assert_eq!(fb.len(), N_BANDS);
        for (_, w) in &fb {
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        }
    }
}
