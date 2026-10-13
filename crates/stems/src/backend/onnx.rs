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
use std::sync::OnceLock;

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

/// Register one CPU arena allocator on the ORT environment, shared by all sessions
/// (`with_env_allocators`). With per-session arenas, the four `htdemucs_ft` sessions each keep
/// their multi-GB peak activation memory, which drove a 16 GB machine into swap; runs are
/// sequential, so one shared arena only needs the peak of a single run.
fn shared_arena() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let res = (|| -> Result<()> {
            use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
            use ort::AsPointer;
            let env = ort::environment::get_environment()?;
            let mi = MemoryInfo::new(AllocationDevice::CPU, 0, AllocatorType::Arena, MemoryType::Default)?;
            let api = ort::api();
            let f = api.CreateAndRegisterAllocator.ok_or_else(|| anyhow::anyhow!("no CreateAndRegisterAllocator"))?;
            // SAFETY: valid env / memory-info pointers; a null arena config selects the defaults.
            let status = unsafe { f(env.ptr().cast_mut(), mi.ptr(), std::ptr::null()) };
            if !status.is_null() {
                let msg = unsafe {
                    let m = api.GetErrorMessage.map(|g| g(status)).unwrap_or(std::ptr::null());
                    let s = if m.is_null() { "unknown".to_string() } else { std::ffi::CStr::from_ptr(m).to_string_lossy().into_owned() };
                    if let Some(r) = api.ReleaseStatus {
                        r(status);
                    }
                    s
                };
                anyhow::bail!("CreateAndRegisterAllocator: {msg}");
            }
            Ok(())
        })();
        match res {
            Ok(()) => true,
            Err(e) => {
                log::warn!("onnxruntime shared arena unavailable ({e:#}); using per-session arenas");
                false
            }
        }
    })
}

impl OrtBackend {
    pub fn load(man: &Manifest) -> Result<Self> {
        let shared = shared_arena();
        let mut sessions = Vec::new();
        for name in &man.models {
            let path = man.dir.join(format!("{name}.onnx"));
            if !path.exists() {
                bail!("missing {}", path.display());
            }
            let mut b = Session::builder()?;
            if shared {
                b = b.with_env_allocators()?;
            }
            let s = b
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

/// An ONNX model with one f32 input and one f32 output of fixed shapes.
pub struct OrtSingle {
    session: Session,
    input: (String, Vec<usize>),
    output: (String, Vec<usize>),
}

impl OrtSingle {
    pub fn load(path: &std::path::Path, input: (&str, &[usize]), output: (&str, &[usize])) -> Result<Self> {
        if !path.exists() {
            bail!("missing {}", path.display());
        }
        let shared = shared_arena();
        let mut b = Session::builder()?;
        if shared {
            b = b.with_env_allocators()?;
        }
        let session = b
            .with_optimization_level(opt_level())?
            .with_intra_threads(threads())?
            .commit_from_file(path)
            .with_context(|| format!("load {}", path.display()))?;
        Ok(OrtSingle { session, input: (input.0.into(), input.1.to_vec()), output: (output.0.into(), output.1.to_vec()) })
    }
}

impl super::SingleBackend for OrtSingle {
    fn name(&self) -> &'static str {
        "onnxruntime-cpu"
    }

    fn run(&self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let t = Tensor::from_array((self.input.1.clone(), input))?;
        let outputs = self.session.run(ort::inputs![self.input.0.as_str() => t]?)?;
        let (shape, data) = outputs[self.output.0.as_str()].try_extract_raw_tensor::<f32>()?;
        let want: Vec<i64> = self.output.1.iter().map(|&d| d as i64).collect();
        if shape != want.as_slice() {
            bail!("unexpected output shape {shape:?}");
        }
        out.clear();
        out.extend_from_slice(data);
        Ok(())
    }
}
