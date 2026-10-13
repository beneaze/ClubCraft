//! Checks the Rust STFT and the backends against reference tensors exported from PyTorch
//! (`models/htdemucs_ref_*.npy`). Skipped when the models are not present.

use clubcraft_stems::backend::{self, BackendKind, SourceOut};
use clubcraft_stems::npy::read_npy;
use clubcraft_stems::stft::Spectro;
use clubcraft_stems::Manifest;
use std::path::PathBuf;

fn models() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models");
    d.join("htdemucs_ref_mix.npy").exists().then_some(d)
}

fn sdr(reference: &[f32], est: &[f32]) -> f64 {
    let num: f64 = reference.iter().map(|v| (*v as f64).powi(2)).sum();
    let den: f64 = reference.iter().zip(est).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum();
    10.0 * (num / den.max(1e-30)).log10()
}

#[test]
fn spec_matches_torch() {
    let Some(dir) = models() else { return };
    let mix = read_npy(&dir.join("htdemucs_ref_mix.npy")).unwrap();
    let want = read_npy(&dir.join("htdemucs_ref_spec.npy")).unwrap();
    let l = mix.shape[2];
    let s = Spectro::new(4096, 1024);
    let mut got = vec![0f32; want.data.len()];
    s.spec(&[&mix.data[..l], &mix.data[l..]], &mut got);
    let err = got.iter().zip(&want.data).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    println!("spec max abs err {err:.3e}");
    assert!(err < 1e-4, "spec max abs err {err}");
}

fn full_model(kind: BackendKind) {
    let Some(dir) = models() else { return };
    let man = Manifest::load(&dir, "htdemucs").unwrap();
    let be = match backend::load(kind, &man) {
        Ok(b) => b,
        Err(e) => panic!("backend {kind:?}: {e:#}"),
    };
    let mix = read_npy(&dir.join("htdemucs_ref_mix.npy")).unwrap();
    let want = read_npy(&dir.join("htdemucs_ref_out.npy")).unwrap();
    let l = man.segment_samples;
    let s = Spectro::new(man.nfft, man.hop_length);
    let mut spec = vec![0f32; man.spec_len()];
    s.spec(&[&mix.data[..l], &mix.data[l..]], &mut spec);
    let srcs = [0usize, 1, 2, 3];
    let mut outs = vec![SourceOut::default(); 4];
    let t0 = std::time::Instant::now();
    be.run(0, &mix.data, &spec, &srcs, &mut outs).unwrap();
    let t1 = std::time::Instant::now();
    be.run(0, &mix.data, &spec, &srcs, &mut outs).unwrap();
    println!("{}: first run {:.2}s, second {:.2}s", be.name(), (t1 - t0).as_secs_f64(), t1.elapsed().as_secs_f64());
    let plane = man.freqs() * man.spec_frames;
    for (k, o) in outs.iter().enumerate() {
        let mut y = vec![0f32; 2 * l];
        for ch in 0..2 {
            let yc = &mut y[ch * l..(ch + 1) * l];
            s.ispec_channel(&o.spec[2 * ch * plane..(2 * ch + 1) * plane], &o.spec[(2 * ch + 1) * plane..(2 * ch + 2) * plane], yc);
            for (v, w) in yc.iter_mut().zip(&o.wave[ch * l..(ch + 1) * l]) {
                *v += w;
            }
        }
        let r = &want.data[k * 2 * l..(k + 1) * 2 * l];
        let d = sdr(r, &y);
        println!("{}: source {k} SDR vs torch {d:.1} dB", be.name());
        // CoreML reaches 76-129 dB here. ORT 1.20 CPU gives 61/51/105/61 dB on this synthetic sine
        // mix (identical in Python onnxruntime, any optimisation level); on real music ORT is 78-88 dB
        // and CoreML 110-124 dB vs PyTorch.
        let min = if kind == BackendKind::Ort { 45.0 } else { 60.0 };
        assert!(d > min, "source {k}: {d} dB");
    }
}

#[test]
fn ort_matches_torch() {
    full_model(BackendKind::Ort);
}

#[cfg(target_os = "macos")]
#[test]
fn coreml_matches_torch() {
    full_model(BackendKind::CoreMl);
}
