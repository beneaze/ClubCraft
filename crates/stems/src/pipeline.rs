//! Demucs `apply_model` semantics: segmenting with overlap and triangular cross-fades, the shift
//! trick, bag-of-models weighting — executed as a three-stage pipeline (STFT prep | inference |
//! iSTFT + overlap-add) so the GPU never waits for the CPU work.
//!
//! Equivalence with Demucs (see `demucs/apply.py`): for a shift `sh` (Demucs: `max_shift - offset`),
//! the "shifted" signal is `x(s - sh)` over `s ∈ [0, len + sh)`. It is cut into segments starting at
//! `k * stride` (`stride = int((1 - overlap) * L)`), each of length `l_k = min(L, len + sh - s_k)`.
//! The model sees an `L`-sample window centred on that segment (zeros outside the track), its output
//! is centre-trimmed back to `l_k`, weighted by `weight[..l_k]` and normalised by the per-sample sum of
//! weights. Shifts are averaged, then sources are combined across models with the manifest weights.

use crate::backend::{Backend, SourceOut};
use crate::model::Manifest;
use crate::stft::Spectro;
use anyhow::{anyhow, Result};
use clubcraft_core::{StemTrack, STEM_CHANNELS, STEM_CHUNK_FRAMES};
use std::sync::mpsc::{channel, sync_channel, Receiver};

/// One model invocation site: segment `k` of the grid of shift `j`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window {
    pub j: usize,
    pub k: usize,
}

pub(crate) struct Plan {
    pub len: usize,
    pub seg: usize,
    pub stride: usize,
    pub weight: Vec<f32>,
    /// Shift (in samples, `max_shift - demucs_offset`) of each pass.
    pub shifts: Vec<usize>,
    /// Per shift: `(start in shifted coordinates, length)` of every segment.
    pub grids: Vec<Vec<(usize, usize)>>,
    pub windows: Vec<Window>,
}

impl Plan {
    pub fn new(len: usize, seg: usize, overlap: f64, shifts: &[usize]) -> Plan {
        let stride = (((1.0 - overlap) * seg as f64) as usize).max(1);
        let half = seg / 2;
        let wmax = half.max(seg - half) as f32;
        let weight: Vec<f32> =
            (1..=half).chain((1..=seg - half).rev()).map(|v| v as f32 / wmax).collect();
        let shifts = if shifts.is_empty() { vec![0] } else { shifts.to_vec() };
        let mut grids = Vec::new();
        let mut windows = Vec::new();
        for (j, &sh) in shifts.iter().enumerate() {
            let total = len + sh;
            let grid: Vec<(usize, usize)> =
                (0..).map(|k| k * stride).take_while(|&s| s < total).map(|s| (s, seg.min(total - s))).collect();
            for k in 0..grid.len() {
                windows.push(Window { j, k });
            }
            grids.push(grid);
        }
        let mut p = Plan { len, seg, stride, weight, shifts, grids, windows: Vec::new() };
        p.windows = windows.into_iter().filter(|w| {
            let (a, b) = p.out_range(*w);
            a < b
        }).collect();
        p
    }

    /// Output samples of a window in track coordinates `[a, b)`.
    pub fn out_range(&self, w: Window) -> (usize, usize) {
        let sh = self.shifts[w.j] as isize;
        let (s, l) = self.grids[w.j][w.k];
        let a = (s as isize - sh).max(0) as usize;
        let b = ((s + l) as isize - sh).clamp(0, self.len as isize) as usize;
        (a.min(b), b)
    }

    /// Start (track coordinates, may be negative) of the `L`-sample model input of a window.
    pub fn input_start(&self, w: Window) -> isize {
        let (s, l) = self.grids[w.j][w.k];
        s as isize - ((self.seg - l) / 2) as isize - self.shifts[w.j] as isize
    }

    /// Sum of the overlap-add weights at shifted position `s` of grid `j`.
    fn denom(&self, j: usize, k: usize, s: usize) -> f32 {
        let grid = &self.grids[j];
        let reach = self.seg.div_ceil(self.stride) + 1;
        let mut sum = 0f32;
        for (st, l) in &grid[k.saturating_sub(reach)..(k + reach + 1).min(grid.len())] {
            if s >= *st && s < st + l {
                sum += self.weight[s - st];
            }
        }
        sum
    }

    /// Window processing order: windows overlapping `start_frame`'s stem chunk first, then forward,
    /// then wrapping around to the beginning.
    pub fn order(&self, start_frame: usize) -> Vec<usize> {
        let c0 = (start_frame.min(self.len.saturating_sub(1)) / STEM_CHUNK_FRAMES) * STEM_CHUNK_FRAMES;
        let mut idx: Vec<usize> = (0..self.windows.len()).collect();
        let key = |i: &usize| {
            let (a, b) = self.out_range(self.windows[*i]);
            let k = if b > c0 { a } else { a + self.len };
            (k, self.windows[*i].j)
        };
        idx.sort_by_key(key);
        idx
    }
}

/// Planar stem storage split into `STEM_CHUNK_FRAMES` chunks, each `[4 stems][2 ch][frames]`,
/// allocated on first write and released when handed to a [`StemTrack`].
pub(crate) struct ChunkedStems {
    pub len: usize,
    pub chunks: Vec<Option<Vec<f32>>>,
}

impl ChunkedStems {
    fn new(len: usize) -> Self {
        ChunkedStems { len, chunks: (0..len.div_ceil(STEM_CHUNK_FRAMES)).map(|_| None).collect() }
    }

    fn chunk_frames(&self, c: usize) -> usize {
        (self.len - c * STEM_CHUNK_FRAMES).min(STEM_CHUNK_FRAMES)
    }

    fn chunk_mut(&mut self, c: usize) -> &mut Vec<f32> {
        let n = self.chunk_frames(c);
        self.chunks[c].get_or_insert_with(|| vec![0f32; 8 * n])
    }

    /// Apply `x * scale + offset` to chunk `c` and return it interleaved (8 channels).
    fn finish_interleaved(&mut self, c: usize, scale: f32, offset: f32) -> Vec<f32> {
        let n = self.chunk_frames(c);
        let planar = self.chunks[c].take().unwrap_or_else(|| vec![0f32; 8 * n]);
        let mut out = vec![0f32; n * STEM_CHANNELS];
        for ch in 0..8 {
            let src = &planar[ch * n..(ch + 1) * n];
            for (f, v) in src.iter().enumerate() {
                out[f * STEM_CHANNELS + ch] = v * scale + offset;
            }
        }
        out
    }

    /// Assemble planar stems (applying `x * scale + offset`), freeing chunks as it goes.
    pub fn into_planar(mut self, scale: f32, offset: f32) -> [(Vec<f32>, Vec<f32>); 4] {
        let mut out: [(Vec<f32>, Vec<f32>); 4] = Default::default();
        for (l, r) in out.iter_mut() {
            l.reserve_exact(self.len);
            r.reserve_exact(self.len);
        }
        for c in 0..self.chunks.len() {
            let n = self.chunk_frames(c);
            let planar = self.chunks[c].take().unwrap_or_else(|| vec![0f32; 8 * n]);
            for (s, (l, r)) in out.iter_mut().enumerate() {
                l.extend(planar[(2 * s) * n..(2 * s + 1) * n].iter().map(|v| v * scale + offset));
                r.extend(planar[(2 * s + 1) * n..(2 * s + 2) * n].iter().map(|v| v * scale + offset));
            }
        }
        out
    }
}

pub(crate) struct Job<'a> {
    pub man: &'a Manifest,
    pub backend: &'a dyn Backend,
    pub spectro: &'a Spectro,
    /// Normalised track, planar.
    pub x: [&'a [f32]; 2],
    pub plan: &'a Plan,
    pub order: Vec<usize>,
    /// Un-normalisation applied to finished samples: `y * scale + offset`.
    pub scale: f32,
    pub offset: f32,
    pub target: Option<&'a StemTrack>,
}

struct Prepared {
    win: usize,
    mix: Vec<f32>,
    spec: Vec<f32>,
}

struct Inferred {
    win: usize,
    model: usize,
    outs: Vec<SourceOut>,
}

impl Job<'_> {
    /// Run all windows; `progress(fraction)` returning false cancels. Returns `None` on cancel.
    pub fn run(&self, progress: &mut dyn FnMut(f32) -> bool) -> Result<Option<ChunkedStems>> {
        let man = self.man;
        let plan = self.plan;
        let l = man.segment_samples;
        let nmodels = man.models.len();
        let used: Vec<Vec<usize>> = (0..nmodels).map(|i| man.used_sources(i)).collect();
        let total_steps = (self.order.len() * nmodels).max(1) as f32;

        // Chunk completion bookkeeping: number of windows still to come per stem chunk.
        let nchunks = plan.len.div_ceil(STEM_CHUNK_FRAMES);
        let mut pending = vec![0usize; nchunks];
        for &wi in &self.order {
            let (a, b) = plan.out_range(plan.windows[wi]);
            for c in a / STEM_CHUNK_FRAMES..b.div_ceil(STEM_CHUNK_FRAMES) {
                pending[c] += 1;
            }
        }

        std::thread::scope(|scope| -> Result<Option<ChunkedStems>> {
            // Stage 1: build model inputs (mix window + spectrogram).
            let (prep_tx, prep_rx) = sync_channel::<Prepared>(1);
            let (recycle_tx, recycle_rx) = channel::<(Vec<f32>, Vec<f32>)>();
            let order = &self.order;
            let prep = scope.spawn(move || {
                for &wi in order {
                    let w = plan.windows[wi];
                    let (mut mix, mut spec) = recycle_rx.try_recv().unwrap_or_default();
                    mix.clear();
                    mix.resize(2 * l, 0.0);
                    let start = plan.input_start(w);
                    for c in 0..2 {
                        let dst = &mut mix[c * l..(c + 1) * l];
                        let src = self.x[c];
                        let a = start.max(0) as usize;
                        let b = ((start + l as isize).max(0) as usize).min(src.len());
                        if a < b {
                            let off = (a as isize - start) as usize;
                            dst[off..off + (b - a)].copy_from_slice(&src[a..b]);
                        }
                    }
                    spec.resize(man.spec_len(), 0.0);
                    self.spectro.spec(&[&mix[..l], &mix[l..]], &mut spec);
                    if prep_tx.send(Prepared { win: wi, mix, spec }).is_err() {
                        break;
                    }
                }
            });

            // Stage 3: iSTFT, overlap-add, chunk publishing.
            let (inf_tx, inf_rx) = sync_channel::<Inferred>(2);
            let (outs_tx, outs_rx) = channel::<Vec<SourceOut>>();
            let post = scope.spawn(move || -> Result<ChunkedStems> {
                self.post_stage(inf_rx, outs_tx, pending, nmodels)
            });

            // Stage 2 (this thread): inference.
            let mut done = 0usize;
            let mut cancelled = false;
            let mut failure: Option<anyhow::Error> = None;
            'outer: for p in prep_rx.iter() {
                for (i, srcs) in used.iter().enumerate() {
                    let mut outs = outs_rx.try_recv().unwrap_or_default();
                    outs.resize_with(srcs.len(), SourceOut::default);
                    outs.truncate(srcs.len());
                    if let Err(e) = self.backend.run(i, &p.mix, &p.spec, srcs, &mut outs) {
                        failure = Some(e);
                        break 'outer;
                    }
                    if inf_tx.send(Inferred { win: p.win, model: i, outs }).is_err() {
                        break 'outer; // post stage failed; its error is reported below
                    }
                    done += 1;
                    if !progress(done as f32 / total_steps) {
                        cancelled = true;
                        break 'outer;
                    }
                }
                let _ = recycle_tx.send((p.mix, p.spec));
            }
            drop(prep_rx);
            drop(inf_tx);
            prep.join().map_err(|_| anyhow!("prep thread panicked"))?;
            let res = post.join().map_err(|_| anyhow!("post thread panicked"))?;
            if let Some(e) = failure {
                return Err(e);
            }
            if cancelled {
                return Ok(None);
            }
            Ok(Some(res?))
        })
    }

    fn post_stage(
        &self,
        rx: Receiver<Inferred>,
        recycle: std::sync::mpsc::Sender<Vec<SourceOut>>,
        mut pending: Vec<usize>,
        nmodels: usize,
    ) -> Result<ChunkedStems> {
        let man = self.man;
        let plan = self.plan;
        let l = man.segment_samples;
        let nsrc = man.sources.len();
        let totals: Vec<f32> = (0..nsrc).map(|s| man.weights.iter().map(|w| w[s]).sum()).collect();
        let nshifts = plan.shifts.len() as f32;
        let stems_of: Vec<usize> = (0..nsrc).map(|s| man.stem_of_source(s)).collect::<Result<_>>()?;
        let mut acc = ChunkedStems::new(plan.len);
        let mut models_done = vec![0usize; plan.windows.len()];
        let mut ys: Vec<Vec<f32>> = Vec::new();
        let mut coefs = vec![0f32; l];

        for msg in rx.iter() {
            let w = plan.windows[msg.win];
            let srcs = man.used_sources(msg.model);
            // Full waveform per (source, channel): ispec(spec_out) + wave_out.
            let jobs = srcs.len() * 2;
            ys.resize_with(jobs, Vec::new);
            let spectro = self.spectro;
            let plane = man.freqs() * man.spec_frames;
            std::thread::scope(|s| {
                for (q, y) in ys.iter_mut().enumerate().take(jobs) {
                    let out = &msg.outs[q / 2];
                    let ch = q % 2;
                    s.spawn(move || {
                        y.resize(l, 0.0);
                        spectro.ispec_channel(
                            &out.spec[(2 * ch) * plane..(2 * ch + 1) * plane],
                            &out.spec[(2 * ch + 1) * plane..(2 * ch + 2) * plane],
                            y,
                        );
                        for (v, t) in y.iter_mut().zip(&out.wave[ch * l..(ch + 1) * l]) {
                            *v += t;
                        }
                    });
                }
            });

            // Overlap-add weights for this window's output range.
            let (a, b) = plan.out_range(w);
            let (s0, seg_len) = plan.grids[w.j][w.k];
            let sh = plan.shifts[w.j];
            let trim = (plan.seg - seg_len) / 2;
            for t in a..b {
                let s = t + sh;
                coefs[t - a] = plan.weight[s - s0] / plan.denom(w.j, w.k, s);
            }
            for (si, &src) in srcs.iter().enumerate() {
                let c = man.weights[msg.model][src] / totals[src] / nshifts;
                let stem = stems_of[src];
                for ch in 0..2 {
                    let y = &ys[si * 2 + ch];
                    let mut t = a;
                    while t < b {
                        let ci = t / STEM_CHUNK_FRAMES;
                        let n = acc.chunk_frames(ci);
                        let end = b.min(ci * STEM_CHUNK_FRAMES + n);
                        let buf = acc.chunk_mut(ci);
                        let dst = &mut buf[(2 * stem + ch) * n..(2 * stem + ch + 1) * n];
                        for tt in t..end {
                            let yi = trim + (tt + sh - s0);
                            dst[tt - ci * STEM_CHUNK_FRAMES] += c * coefs[tt - a] * y[yi];
                        }
                        t = end;
                    }
                }
            }
            let _ = recycle.send(msg.outs);

            models_done[msg.win] += 1;
            if models_done[msg.win] == nmodels {
                for c in a / STEM_CHUNK_FRAMES..b.div_ceil(STEM_CHUNK_FRAMES) {
                    pending[c] -= 1;
                    if pending[c] == 0 {
                        if let Some(target) = self.target {
                            let data = acc.finish_interleaved(c, self.scale, self.offset);
                            target.set_chunk(c, data);
                        }
                    }
                }
            }
        }
        Ok(acc)
    }
}
