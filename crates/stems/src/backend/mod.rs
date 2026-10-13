//! Inference backends running the exported HTDemucs core graph.

use crate::model::Manifest;
use anyhow::Result;

#[cfg(target_os = "macos")]
pub mod coreml;
pub mod onnx;

/// Output of one model for one source: `spec` is `[4][F][T]` complex-as-channels, `wave` is `[2][L]`.
#[derive(Default, Clone)]
pub struct SourceOut {
    pub spec: Vec<f32>,
    pub wave: Vec<f32>,
}

/// Which inference backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendKind {
    /// CoreML (GPU) on macOS when the compiled models load, otherwise ONNX Runtime CPU.
    #[default]
    Auto,
    /// CoreML ML Program on CPU+GPU (macOS only).
    CoreMl,
    /// ONNX Runtime, CPU execution provider.
    Ort,
}

pub trait Backend: Send + Sync {
    fn name(&self) -> &'static str;
    /// Run sub-model `m` on one segment (`mix` = `[2][L]`, `spec` = `[4][F][T]`) and copy the outputs of
    /// the model sources listed in `sources` into `out` (same order). Safe to call from several
    /// threads at once.
    fn run(&self, m: usize, mix: &[f32], spec: &[f32], sources: &[usize], out: &mut [SourceOut]) -> Result<()>;
}

pub fn load(kind: BackendKind, man: &Manifest) -> Result<Box<dyn Backend>> {
    match kind {
        #[cfg(target_os = "macos")]
        BackendKind::CoreMl => Ok(Box::new(coreml::CoreMlBackend::load(man)?)),
        #[cfg(not(target_os = "macos"))]
        BackendKind::CoreMl => anyhow::bail!("CoreML is only available on macOS"),
        BackendKind::Ort => Ok(Box::new(onnx::OrtBackend::load(man)?)),
        BackendKind::Auto => {
            #[cfg(target_os = "macos")]
            match coreml::CoreMlBackend::load(man) {
                Ok(b) => return Ok(Box::new(b)),
                Err(e) => log::warn!("CoreML backend unavailable ({e:#}); falling back to ONNX Runtime CPU"),
            }
            Ok(Box::new(onnx::OrtBackend::load(man)?))
        }
    }
}
