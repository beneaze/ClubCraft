//! Mel-Band RoFormer vocal separation (Kimberley Jensen's MIT checkpoint, `models/melroformer.*`).
//!
//! The exported graph is `MelBandRoformer.forward` between the band gather and the mask scatter
//! (see `tools/model-export/export_melroformer.py`). This module does the rest exactly like the
//! PyTorch code: `torch.stft(n_fft=2048, hop=441, hann, center=True, reflect, normalized=False)`,
//! gather of the (overlapping) mel-band bins, scatter-average of the predicted complex masks,
//! masking and `torch.istft`. Chunking follows the checkpoint's reference inference
//! (`demix_track`: 8 s chunks, `num_overlap = 2`, linear fades of `chunk / 10`, reflect padding).

use crate::backend::{self, BackendKind, SingleBackend};
use crate::stft::pad1d_reflect;
use anyhow::{bail, Context, Result};
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Deserialize)]
pub struct RoformerManifest {
    pub name: String,
    pub architecture: String,
    pub samplerate: u32,
    pub chunk_samples: usize,
    pub frames: usize,
    pub nfft: usize,
    pub hop_length: usize,
    pub win_length: usize,
    pub num_overlap: usize,
    pub zero_dc: bool,
    pub freq_indices: Vec<usize>,
    pub num_bands_per_freq: Vec<usize>,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl RoformerManifest {
    pub fn load(dir: &Path, name: &str) -> Result<Self> {
        let p = dir.join(format!("{name}.json"));
        let text = std::fs::read_to_string(&p).with_context(|| format!("model manifest {}", p.display()))?;
        let mut m: RoformerManifest = serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
        m.dir = dir.to_path_buf();
        if m.architecture != "mel_band_roformer" || m.win_length != m.nfft {
            bail!("unsupported RoFormer manifest {}", p.display());
        }
        if m.frames != m.chunk_samples / m.hop_length + 1 || m.num_bands_per_freq.len() != m.nfft / 2 + 1 {
            bail!("inconsistent RoFormer manifest {}", p.display());
        }
        if m.freq_indices.iter().any(|&i| i >= 2 * (m.nfft / 2 + 1)) {
            bail!("bad freq_indices in {}", p.display());
        }
        Ok(m)
    }

    /// Width of the gathered input / mask: 2 (complex) per gathered bin.
    pub fn dim(&self) -> usize {
        2 * self.freq_indices.len()
    }
}

/// `torch.stft` / `torch.istft` with `center=True`, reflect padding, periodic Hann window and no
/// normalisation, for any hop size.
pub struct TorchStft {
    pub nfft: usize,
    pub hop: usize,
    window: Vec<f32>,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
}

impl TorchStft {
    pub fn new(nfft: usize, hop: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let window = (0..nfft)
            .map(|n| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / nfft as f64).cos()) as f32)
            .collect();
        TorchStft { nfft, hop, window, r2c: planner.plan_fft_forward(nfft), c2r: planner.plan_fft_inverse(nfft) }
    }

    pub fn bins(&self) -> usize {
        self.nfft / 2 + 1
    }

    pub fn frames(&self, len: usize) -> usize {
        len / self.hop + 1
    }

    /// Spectrum planes `re[f * T + t]`, `im[f * T + t]`.
    pub fn stft(&self, x: &[f32], re: &mut [f32], im: &mut [f32]) {
        let t_frames = self.frames(x.len());
        let padded = pad1d_reflect(x, self.nfft / 2, self.nfft / 2);
        let mut buf = self.r2c.make_input_vec();
        let mut spec = self.r2c.make_output_vec();
        let mut scratch = self.r2c.make_scratch_vec();
        for t in 0..t_frames {
            let frame = &padded[t * self.hop..t * self.hop + self.nfft];
            for ((b, &s), &w) in buf.iter_mut().zip(frame).zip(&self.window) {
                *b = s * w;
            }
            self.r2c.process_with_scratch(&mut buf, &mut spec, &mut scratch).expect("fft");
            for (f, c) in spec.iter().enumerate() {
                re[f * t_frames + t] = c.re;
                im[f * t_frames + t] = c.im;
            }
        }
    }

    /// Inverse of [`TorchStft::stft`]; `out.len()` must be `hop * (T - 1)` (torch's default length).
    pub fn istft(&self, re: &[f32], im: &[f32], t_frames: usize, out: &mut [f32]) {
        let (nfft, hop, nb) = (self.nfft, self.hop, self.bins());
        let length = out.len();
        let pad = nfft / 2;
        let total = nfft + hop * (t_frames - 1);
        let mut ola = vec![0f32; total];
        let mut env = vec![0f32; total];
        let mut spec = self.c2r.make_input_vec();
        let mut frame = self.c2r.make_output_vec();
        let mut scratch = self.c2r.make_scratch_vec();
        let scale = 1.0 / nfft as f32;
        for t in 0..t_frames {
            for f in 0..nb {
                spec[f] = Complex32::new(re[f * t_frames + t], im[f * t_frames + t]);
            }
            spec[0].im = 0.0;
            spec[nb - 1].im = 0.0;
            self.c2r.process_with_scratch(&mut spec, &mut frame, &mut scratch).expect("ifft");
            let o = t * hop;
            for n in 0..nfft {
                ola[o + n] += frame[n] * scale * self.window[n];
                env[o + n] += self.window[n] * self.window[n];
            }
        }
        for (j, v) in out.iter_mut().enumerate().take(length) {
            let e = env[j + pad];
            *v = if e > 1e-11 { ola[j + pad] / e } else { 0.0 };
        }
    }
}

/// A loaded Mel-Band RoFormer vocal model.
pub struct Roformer {
    pub man: RoformerManifest,
    model: Box<dyn SingleBackend>,
    stft: TorchStft,
    /// For each STFT row `2 * bin + ch`: the gathered indices `k` that read it.
    rows: Vec<Vec<u32>>,
}

impl Roformer {
    /// Load the vocal model; on CoreML the half-precision export when present (see below).
    pub fn load(dir: &Path, name: &str, kind: BackendKind) -> Result<Self> {
        Self::load_with(dir, name, kind, std::env::var_os("CLUBCRAFT_ROFORMER_FP32").is_none())
    }

    /// Load the vocal model; `allow_fp16` lets CoreML use `<name>_fp16.mlmodelc` if present.
    pub fn load_with(dir: &Path, name: &str, kind: BackendKind, allow_fp16: bool) -> Result<Self> {
        let man = RoformerManifest::load(dir, name)?;
        let shape = [1, man.frames, man.dim()];
        // On CoreML the half-precision export runs ~1.4x faster on the GPU and matches fp32 to
        // ~66 dB on real music (inaudible); `CLUBCRAFT_ROFORMER_FP32` forces full precision.
        let fp16 = dir.join(format!("{name}_fp16"));
        let use16 = cfg!(target_os = "macos")
            && matches!(kind, BackendKind::Auto | BackendKind::CoreMl)
            && dir.join(format!("{name}_fp16.mlmodelc")).exists()
            && allow_fp16;
        let base = if use16 { fp16 } else { dir.join(name) };
        let model = backend::load_single(kind, &base, ("x", &shape), ("mask", &shape))?;
        let stft = TorchStft::new(man.nfft, man.hop_length);
        let mut rows = vec![Vec::new(); 2 * stft.bins()];
        for (k, &r) in man.freq_indices.iter().enumerate() {
            rows[r].push(k as u32);
        }
        Ok(Roformer { man, model, stft, rows })
    }

    pub fn backend_name(&self) -> &'static str {
        self.model.name()
    }

    /// Model input for one `[2][C]` chunk: (spectrum planes per channel, gathered `[T][D]`).
    pub fn prepare(&self, chunk: [&[f32]; 2]) -> ([Vec<f32>; 4], Vec<f32>) {
        let t = self.man.frames;
        let nb = self.stft.bins();
        let mut planes: [Vec<f32>; 4] = std::array::from_fn(|_| vec![0f32; nb * t]);
        {
            let [re0, im0, re1, im1] = &mut planes;
            std::thread::scope(|s| {
                s.spawn(|| self.stft.stft(chunk[0], re0, im0));
                self.stft.stft(chunk[1], re1, im1);
            });
        }
        let d = self.man.dim();
        let mut x = vec![0f32; t * d];
        for (k, &row) in self.man.freq_indices.iter().enumerate() {
            let (bin, ch) = (row / 2, row % 2);
            let re = &planes[2 * ch][bin * t..(bin + 1) * t];
            let im = &planes[2 * ch + 1][bin * t..(bin + 1) * t];
            for tt in 0..t {
                x[tt * d + 2 * k] = re[tt];
                x[tt * d + 2 * k + 1] = im[tt];
            }
        }
        (planes, x)
    }

    /// Apply the predicted masks and invert: returns the vocals of the chunk, `[2][C]`.
    pub fn finish(&self, planes: &[Vec<f32>; 4], mask: &[f32]) -> [Vec<f32>; 2] {
        let t = self.man.frames;
        let d = self.man.dim();
        let nb = self.stft.bins();
        let out_len = self.stft.hop * (t - 1);
        let mut out: [Vec<f32>; 2] = [vec![0f32; out_len], vec![0f32; out_len]];
        std::thread::scope(|s| {
            for (ch, o) in out.iter_mut().enumerate() {
                s.spawn(move || {
                    let mut re = vec![0f32; nb * t];
                    let mut im = vec![0f32; nb * t];
                    let (zr, zi) = (&planes[2 * ch], &planes[2 * ch + 1]);
                    for bin in 0..nb {
                        let ks = &self.rows[2 * bin + ch];
                        let denom = (self.man.num_bands_per_freq[bin] as f32).max(1e-8);
                        if self.man.zero_dc && bin == 0 {
                            continue;
                        }
                        for tt in 0..t {
                            let (mut mr, mut mi) = (0f32, 0f32);
                            for &k in ks {
                                mr += mask[tt * d + 2 * k as usize];
                                mi += mask[tt * d + 2 * k as usize + 1];
                            }
                            let (mr, mi) = (mr / denom, mi / denom);
                            let i = bin * t + tt;
                            re[i] = zr[i] * mr - zi[i] * mi;
                            im[i] = zr[i] * mi + zi[i] * mr;
                        }
                    }
                    self.stft.istft(&re, &im, t, o);
                });
            }
        });
        out
    }

    /// Vocals of exactly one model chunk (`chunk_samples` per channel).
    pub fn run_chunk(&self, chunk: [&[f32]; 2]) -> Result<[Vec<f32>; 2]> {
        let t0 = std::time::Instant::now();
        let (planes, x) = self.prepare(chunk);
        let t1 = std::time::Instant::now();
        let mut mask = Vec::new();
        self.model.run(&x, &mut mask)?;
        let t2 = std::time::Instant::now();
        let out = self.finish(&planes, &mask);
        log::debug!(
            "roformer chunk: prepare {:.3}s model {:.3}s finish {:.3}s",
            (t1 - t0).as_secs_f64(),
            (t2 - t1).as_secs_f64(),
            t2.elapsed().as_secs_f64()
        );
        Ok(out)
    }

    /// Number of model chunks for a track of `len` frames.
    pub fn chunks_for(&self, len: usize) -> usize {
        VocalPlan::new(&self.man, len).starts.len()
    }

    /// Separate the vocals of a whole track (`demix_track` semantics). `progress(done, total)`
    /// returning false cancels (returns `Ok(None)`).
    pub fn separate(
        &self,
        left: &[f32],
        right: &[f32],
        progress: &mut dyn FnMut(usize, usize) -> bool,
    ) -> Result<Option<[Vec<f32>; 2]>> {
        let pass = VocalPass::new(self, [left, right]);
        let n = pass.plan.starts.len();
        for j in 0..n {
            pass.ensure_chunks(j..j + 1)?;
            if !progress(j + 1, n) {
                return Ok(None);
            }
        }
        Ok(Some(pass.into_vocals()))
    }
}

/// Chunk geometry of `demix_track` for one track.
pub struct VocalPlan {
    pub len: usize,
    pub chunk: usize,
    pub step: usize,
    pub fade: usize,
    /// Reflect padding added on both sides (0 for short tracks).
    pub border: usize,
    /// Padded length.
    pub total: usize,
    /// Chunk starts in padded coordinates.
    pub starts: Vec<usize>,
    window: Vec<f32>,
}

impl VocalPlan {
    pub fn new(man: &RoformerManifest, len: usize) -> Self {
        let c = man.chunk_samples;
        let step = c / man.num_overlap.max(1);
        let fade = c / 10;
        let border = c - step;
        let border = if len > 2 * border && border > 0 { border } else { 0 };
        let total = len + 2 * border;
        let starts = (0..).map(|i| i * step).take_while(|&i| i < total).collect();
        // torch.linspace(0, 1, fade) / linspace(1, 0, fade)
        let mut window = vec![1f32; c];
        for k in 0..fade {
            let v = k as f32 / (fade - 1) as f32;
            window[k] *= v;
            window[c - fade + k] *= 1.0 - v;
        }
        VocalPlan { len, chunk: c, step, fade, border, total, starts, window }
    }

    /// Chunks whose span intersects track frames `[a, b)`.
    pub fn chunks_covering(&self, a: usize, b: usize) -> std::ops::Range<usize> {
        if a >= b {
            return 0..0;
        }
        let (pa, pb) = (a + self.border, b + self.border);
        let first = if pa >= self.chunk { (pa - self.chunk) / self.step + 1 } else { 0 };
        let last = ((pb - 1) / self.step).min(self.starts.len() - 1);
        first..last + 1
    }

    /// Track sample of padded position `p` (reflect padding).
    fn source_index(&self, p: usize) -> usize {
        let i = p as isize - self.border as isize;
        let n = self.len as isize;
        let j = if i < 0 { -i } else if i >= n { 2 * (n - 1) - i } else { i };
        j as usize
    }
}

struct PassState {
    result: [Vec<f32>; 2],
    counter: Vec<f32>,
    done: Vec<bool>,
}

/// An in-progress vocal separation whose chunks can be computed on demand and in any order;
/// accumulation is per sample with at most `num_overlap` terms, so the result does not depend on
/// the order (bit-exact for the default overlap of 2).
pub struct VocalPass<'a> {
    rf: &'a Roformer,
    mix: [&'a [f32]; 2],
    pub plan: VocalPlan,
    state: std::sync::Mutex<PassState>,
    pub chunks_done: std::sync::atomic::AtomicUsize,
}

impl<'a> VocalPass<'a> {
    pub fn new(rf: &'a Roformer, mix: [&'a [f32]; 2]) -> Self {
        let plan = VocalPlan::new(&rf.man, mix[0].len());
        let state = PassState {
            result: [vec![0f32; plan.total], vec![0f32; plan.total]],
            counter: vec![0f32; plan.total],
            done: vec![false; plan.starts.len()],
        };
        VocalPass { rf, mix, plan, state: std::sync::Mutex::new(state), chunks_done: Default::default() }
    }

    /// Compute (if not done yet) the chunks in `range`. Only one thread should call this.
    pub fn ensure_chunks(&self, range: std::ops::Range<usize>) -> Result<()> {
        let c = self.plan.chunk;
        let total = self.plan.total;
        for j in range {
            if self.state.lock().unwrap().done[j] {
                continue;
            }
            let i = self.plan.starts[j];
            let length = (total - i).min(c);
            let part: [Vec<f32>; 2] = std::array::from_fn(|ch| {
                let src = self.mix[ch];
                let mut p: Vec<f32> = (i..i + length).map(|q| src[self.plan.source_index(q)]).collect();
                if length < c {
                    p = if length > c / 2 + 1 { pad1d_reflect(&p, 0, c - length) } else { p.into_iter().chain(std::iter::repeat_n(0.0, c - length)).collect() };
                }
                p
            });
            let y = self.rf.run_chunk([&part[0], &part[1]])?;
            let mut w = self.plan.window.clone();
            if i == 0 {
                w[..self.plan.fade].iter_mut().for_each(|v| *v = 1.0);
            } else if i + c >= total {
                w[c - self.plan.fade..].iter_mut().for_each(|v| *v = 1.0);
            }
            let mut st = self.state.lock().unwrap();
            for ch in 0..2 {
                for k in 0..length {
                    st.result[ch][i + k] += y[ch][k] * w[k];
                }
            }
            for k in 0..length {
                st.counter[i + k] += w[k];
            }
            st.done[j] = true;
            self.chunks_done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// Make the vocals of track frames `[a, b)` final.
    pub fn ensure_range(&self, a: usize, b: usize) -> Result<()> {
        self.ensure_chunks(self.plan.chunks_covering(a, b))
    }

    /// Final vocals of track frames `[a, a + out[ch].len())` (must have been ensured).
    pub fn read(&self, a: usize, out: [&mut [f32]; 2]) {
        let st = self.state.lock().unwrap();
        for (ch, o) in out.into_iter().enumerate() {
            for (k, v) in o.iter_mut().enumerate() {
                let p = a + k + self.plan.border;
                let x = st.result[ch][p] / st.counter[p];
                *v = if x.is_finite() { x } else { 0.0 };
            }
        }
    }

    pub fn into_vocals(self) -> [Vec<f32>; 2] {
        let len = self.plan.len;
        let mut out = [vec![0f32; len], vec![0f32; len]];
        let [l, r] = &mut out;
        self.read(0, [l, r]);
        out
    }
}
