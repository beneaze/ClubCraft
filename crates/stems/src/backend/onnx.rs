//! ONNX Runtime CPU backend (portable fallback). The CoreML execution provider of ORT produces
//! NaNs for this graph, so only the CPU provider is used.

use super::{Backend, SourceOut};
use crate::model::Manifest;
use anyhow::{anyhow, bail, Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::TensorRef;
use std::sync::{Mutex, OnceLock};

pub struct OrtBackend {
    sessions: Vec<Mutex<Session>>,
    l: usize,
    f: usize,
    t: usize,
    s: usize,
}

fn threads() -> usize {
    if let Some(n) = std::env::var("CLUBCRAFT_ORT_THREADS").ok().and_then(|v| v.parse().ok()) {
        return n;
    }
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8)
}

/// ORT allows a single environment per process; keep it alive for the whole process.
fn environment() -> Result<&'static ort::environment::Environment> {
    static ENV: OnceLock<std::result::Result<ort::environment::Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| ort::init().with_name("clubcraft").build().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| anyhow!("onnxruntime init: {e}"))
}

impl OrtBackend {
    pub fn load(man: &Manifest) -> Result<Self> {
        let env = environment()?;
        let mut sessions = Vec::new();
        for name in &man.models {
            let path = man.dir.join(format!("{name}.onnx"));
            if !path.exists() {
                bail!("missing {}", path.display());
            }
            let s = Session::builder(env)
                .map_err(|e| anyhow!("{e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| anyhow!("{e}"))?
                .with_intra_threads(threads())
                .map_err(|e| anyhow!("{e}"))?
                .commit_from_file(&path)
                .with_context(|| format!("load {}", path.display()))?;
            sessions.push(Mutex::new(s));
        }
        Ok(OrtBackend {
            sessions,
            l: man.segment_samples,
            f: man.freqs(),
            t: man.spec_frames,
            s: man.sources.len(),
        })
    }
}

impl Backend for OrtBackend {
    fn name(&self) -> &'static str {
        "onnxruntime-cpu"
    }

    fn run(&self, m: usize, mix: &[f32], spec: &[f32], sources: &[usize], out: &mut [SourceOut]) -> Result<()> {
        let (l, f, t, s) = (self.l, self.f, self.t, self.s);
        let mix_t = TensorRef::from_array_view(([1usize, 2, l], mix))?;
        let spec_t = TensorRef::from_array_view(([1usize, 4, f, t], spec))?;
        let mut sess = self.sessions[m].lock().unwrap();
        let outputs = sess.run(ort::inputs!["mix" => mix_t, "spec" => spec_t])?;
        let (sshape, sdata) = outputs["spec_out"].try_extract_tensor::<f32>()?;
        let (wshape, wdata) = outputs["wave_out"].try_extract_tensor::<f32>()?;
        if sshape.as_ref() != [1, s as i64, 4, f as i64, t as i64] || wshape.as_ref() != [1, s as i64, 2, l as i64] {
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
