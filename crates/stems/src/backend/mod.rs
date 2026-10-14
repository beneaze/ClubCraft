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
    /// ONNX Runtime with DirectML (GPU, Windows only).
    DirectMl,
}

/// DirectML can silently produce wrong results for some graphs (ORT's CoreML provider returns
/// NaNs for HTDemucs on macOS). Before trusting it, compare one run against the CPU provider;
/// the verdict is remembered per model in the ClubCraft data folder.
#[cfg(windows)]
fn directml_verdict(tag: &str, check: &dyn Fn() -> Result<f64>) -> bool {
    let path = clubcraft_core::paths::data_dir().join(format!("directml-{tag}.txt"));
    if let Ok(v) = std::fs::read_to_string(&path) {
        return v.trim() == "ok";
    }
    let ok = match check() {
        Ok(sdr) => {
            log::info!("DirectML self-check for {tag}: {sdr:.1} dB vs CPU");
            sdr.is_finite() && sdr > 40.0
        }
        Err(e) => {
            log::warn!("DirectML self-check for {tag} failed: {e:#}");
            false
        }
    };
    let _ = std::fs::create_dir_all(clubcraft_core::paths::data_dir());
    let _ = std::fs::write(&path, if ok { "ok" } else { "bad" });
    ok
}

/// SDR (dB) of `b` against reference `a`; NaN when `b` contains non-finite values.
#[cfg_attr(not(windows), allow(dead_code))]
fn sdr(a: &[f32], b: &[f32]) -> f64 {
    if b.iter().any(|v| !v.is_finite()) || a.len() != b.len() {
        return f64::NAN;
    }
    let num: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum();
    let den: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).powi(2)).sum();
    10.0 * (num / den.max(1e-20)).log10()
}

/// Deterministic pseudo-random test signal.
#[cfg_attr(not(windows), allow(dead_code))]
fn test_signal(n: usize, seed: u32) -> Vec<f32> {
    let mut x = seed.wrapping_mul(2654435761).max(1);
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            0.1 * ((i as f32 * 0.013).sin() + (x as f32 / u32::MAX as f32 - 0.5))
        })
        .collect()
}

#[cfg(windows)]
fn try_directml(man: &Manifest) -> Option<onnx::OrtBackend> {
    let tag = man.name.clone();
    let gpu = match onnx::OrtBackend::load_with(man, true) {
        Ok(b) => b,
        Err(e) => {
            log::info!("DirectML unavailable ({e:#})");
            return None;
        }
    };
    let ok = directml_verdict(&tag, &|| {
        let cpu = onnx::OrtBackend::load(man)?;
        let mix = test_signal(2 * man.segment_samples, 1);
        let spec = test_signal(4 * man.freqs() * man.spec_frames, 2);
        let mut a = vec![SourceOut::default()];
        let mut b = vec![SourceOut::default()];
        cpu.run(0, &mix, &spec, &[0], &mut a)?;
        gpu.run(0, &mix, &spec, &[0], &mut b)?;
        Ok(sdr(&a[0].wave, &b[0].wave).min(sdr(&a[0].spec, &b[0].spec)))
    });
    ok.then_some(gpu)
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
        BackendKind::DirectMl => Ok(Box::new(onnx::OrtBackend::load_with(man, true)?)),
        BackendKind::Auto => {
            #[cfg(target_os = "macos")]
            match coreml::CoreMlBackend::load(man) {
                Ok(b) => return Ok(Box::new(b)),
                Err(e) => log::warn!("CoreML backend unavailable ({e:#}); falling back to ONNX Runtime CPU"),
            }
            #[cfg(windows)]
            if let Some(b) = try_directml(man) {
                return Ok(Box::new(b));
            }
            Ok(Box::new(onnx::OrtBackend::load(man)?))
        }
    }
}

/// A model with one f32 input and one f32 output of fixed shapes (used for Mel-Band RoFormer).
pub trait SingleBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, input: &[f32], out: &mut Vec<f32>) -> Result<()>;
}

/// Load `<base>.mlmodelc` (CoreML) or `<base>.onnx` (ORT) according to `kind`.
pub fn load_single(
    kind: BackendKind,
    base: &std::path::Path,
    input: (&str, &[usize]),
    output: (&str, &[usize]),
) -> Result<Box<dyn SingleBackend>> {
    let with_ext = |e: &str| {
        let mut p = base.as_os_str().to_owned();
        p.push(e);
        std::path::PathBuf::from(p)
    };
    match kind {
        #[cfg(target_os = "macos")]
        BackendKind::CoreMl => Ok(Box::new(coreml::CoreMlSingle::load(&with_ext(".mlmodelc"), input, output)?)),
        #[cfg(not(target_os = "macos"))]
        BackendKind::CoreMl => anyhow::bail!("CoreML is only available on macOS"),
        BackendKind::Ort => Ok(Box::new(onnx::OrtSingle::load(&with_ext(".onnx"), input, output)?)),
        BackendKind::DirectMl => Ok(Box::new(onnx::OrtSingle::load_with(&with_ext(".onnx"), input, output, true)?)),
        BackendKind::Auto => {
            #[cfg(target_os = "macos")]
            match coreml::CoreMlSingle::load(&with_ext(".mlmodelc"), input, output) {
                Ok(b) => return Ok(Box::new(b)),
                Err(e) => log::warn!("CoreML unavailable for {} ({e:#}); using ONNX Runtime", base.display()),
            }
            #[cfg(windows)]
            {
                let onnx = with_ext(".onnx");
                if let Ok(gpu) = onnx::OrtSingle::load_with(&onnx, input, output, true) {
                    let tag = base.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    let n: usize = input.1.iter().product();
                    let ok = directml_verdict(&tag, &|| {
                        let cpu = onnx::OrtSingle::load(&onnx, input, output)?;
                        let x = test_signal(n, 3);
                        let (mut a, mut b) = (Vec::new(), Vec::new());
                        cpu.run(&x, &mut a)?;
                        gpu.run(&x, &mut b)?;
                        Ok(sdr(&a, &b))
                    });
                    if ok {
                        return Ok(Box::new(gpu));
                    }
                }
            }
            Ok(Box::new(onnx::OrtSingle::load(&with_ext(".onnx"), input, output)?))
        }
    }
}
