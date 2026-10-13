//! ONNX Runtime CPU backend (portable fallback).
//!
//! Pinned to `ort 2.0.0-rc.9` = ONNX Runtime 1.20: the 1.31 static build that newer `ort` releases
//! download returns all-NaN outputs for this graph on real music (and is ~10x slower). The CoreML
//! execution provider of ORT also produces NaNs here, so only the CPU provider is used.

use super::{Backend, SourceOut};
use crate::model::Manifest;
use anyhow::{bail, Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;

pub struct OrtBackend {
    sessions: Vec<Session>,
    l: usize,
    f: usize,
    t: usize,
    s: usize,
}

/// Intra-op threads (`CLUBCRAFT_ORT_THREADS` overrides). The performance cores of Apple silicon
/// scale up to ~8 threads for this model.
fn threads() -> usize {
    if let Some(n) = std::env::var("CLUBCRAFT_ORT_THREADS").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8)
}

fn opt_level() -> GraphOptimizationLevel {
    match std::env::var("CLUBCRAFT_ORT_OPT").ok().as_deref() {
        Some("0") => GraphOptimizationLevel::Disable,
        Some("1") => GraphOptimizationLevel::Level1,
        Some("2") => GraphOptimizationLevel::Level2,
        _ => GraphOptimizationLevel::Level3,
    }
}

impl OrtBackend {
    pub fn load(man: &Manifest) -> Result<Self> {
        let mut sessions = Vec::new();
        for name in &man.models {
            let path = man.dir.join(format!("{name}.onnx"));
            if !path.exists() {
                bail!("missing {}", path.display());
            }
            let s = Session::builder()?
                .with_optimization_level(opt_level())?
                .with_intra_threads(threads())?
                .commit_from_file(&path)
                .with_context(|| format!("load {}", path.display()))?;
            sessions.push(s);
        }
        Ok(OrtBackend { sessions, l: man.segment_samples, f: man.freqs(), t: man.spec_frames, s: man.sources.len() })
    }
}

impl Backend for OrtBackend {
    fn name(&self) -> &'static str {
        "onnxruntime-cpu"
    }

    fn run(&self, m: usize, mix: &[f32], spec: &[f32], sources: &[usize], out: &mut [SourceOut]) -> Result<()> {
        let (l, f, t, s) = (self.l, self.f, self.t, self.s);
        let mix_t = Tensor::from_array(([1usize, 2, l], mix))?;
        let spec_t = Tensor::from_array(([1usize, 4, f, t], spec))?;
        let outputs = self.sessions[m].run(ort::inputs!["mix" => mix_t, "spec" => spec_t]?)?;
        let (sshape, sdata) = outputs["spec_out"].try_extract_raw_tensor::<f32>()?;
        let (wshape, wdata) = outputs["wave_out"].try_extract_raw_tensor::<f32>()?;
        if sshape != [1, s as i64, 4, f as i64, t as i64] || wshape != [1, s as i64, 2, l as i64] {
            bail!("unexpected output shapes {sshape:?} {wshape:?}");
        }
        let (sp, wp) = (4 * f * t, 2 * l);
        for (o, &src) in out.iter_mut().zip(sources) {
            o.spec.clear();
            o.spec.extend_from_slice(&sdata[src * sp..(src + 1) * sp]);
            o.wave.clear();
            o.wave.extend_from_slice(&wdata[src * wp..(src + 1) * wp]);
        }
        Ok(())
    }
}
