//! Mel-Band RoFormer: Rust STFT/gather/scatter/iSTFT and the backends against PyTorch references
//! (`models/melroformer_ref_*.npy`, written by `export_melroformer.py --reference`).

use clubcraft_stems::npy::read_npy;
use clubcraft_stems::roformer::Roformer;
use clubcraft_stems::BackendKind;
use std::path::PathBuf;

fn models() -> Option<PathBuf> {
    let d = std::env::var("CLUBCRAFT_TEST_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models"));
    d.join("melroformer_ref_out.npy").exists().then_some(d)
}

fn sdr(r: &[f32], e: &[f32]) -> f64 {
    let num: f64 = r.iter().map(|v| (*v as f64).powi(2)).sum();
    let den: f64 = r.iter().zip(e).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum();
    10.0 * (num / den.max(1e-30)).log10()
}

fn check(kind: BackendKind, min_db: f64) {
    let Some(dir) = models() else { return };
    let rf = Roformer::load(&dir, "melroformer", kind).unwrap();
    let mix = read_npy(&dir.join("melroformer_ref_mix.npy")).unwrap();
    let x_ref = read_npy(&dir.join("melroformer_ref_x.npy")).unwrap();
    let mask_ref = read_npy(&dir.join("melroformer_ref_mask.npy")).unwrap();
    let out_ref = read_npy(&dir.join("melroformer_ref_out.npy")).unwrap();
    let c = mix.shape[2];
    let (planes, x) = rf.prepare([&mix.data[..c], &mix.data[c..]]);
    let xerr = x.iter().zip(&x_ref.data).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    println!("gathered STFT max abs err {xerr:.2e}");
    assert!(xerr < 1e-3);
    let y = rf.finish(&planes, &mask_ref.data);
    for ch in 0..2 {
        let d = sdr(&out_ref.data[ch * c..(ch + 1) * c], &y[ch]);
        println!("scatter + iSTFT with torch masks, ch {ch}: {d:.1} dB");
        assert!(d > 90.0);
    }
    let t = std::time::Instant::now();
    let y = rf.run_chunk([&mix.data[..c], &mix.data[c..]]).unwrap();
    let t1 = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    let _ = rf.run_chunk([&mix.data[..c], &mix.data[c..]]).unwrap();
    println!("{}: chunk {t1:.2}s first, {:.2}s second", rf.backend_name(), t.elapsed().as_secs_f64());
    for ch in 0..2 {
        let d = sdr(&out_ref.data[ch * c..(ch + 1) * c], &y[ch]);
        println!("{}: full chunk vs torch, ch {ch}: {d:.1} dB", rf.backend_name());
        assert!(d > min_db, "{d}");
    }
}

#[test]
fn roformer_ort() {
    check(BackendKind::Ort, 60.0);
}

#[cfg(target_os = "macos")]
#[test]
fn roformer_coreml() {
    check(BackendKind::CoreMl, 60.0);
}
