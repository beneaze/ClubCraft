//! Developer check: run one segment through a backend and print timing and output statistics.
//! `cargo run --release -p clubcraft-stems --example backend_check -- [coreml|ort] [input.npy [offset_s]]`
//! or `-- roformer [seconds]` (vocal model alone; `RUST_LOG`-style debug timing via the `log` crate).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine
use clubcraft_stems::backend::{self, BackendKind, SourceOut};
use clubcraft_stems::{npy, stft::Spectro, Manifest};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("roformer") {
        // Vocal model alone on N seconds of noise-like signal: per-chunk timing.
        let dir = clubcraft_core::paths::models_dir();
        let rf = clubcraft_stems::Roformer::load(&dir, "melroformer", BackendKind::CoreMl)?;
        let n = 44100 * args.get(2).map(|s| s.parse::<usize>().unwrap()).unwrap_or(30);
        let x: Vec<f32> = (0..n).map(|i| ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.3).collect();
        let t = std::time::Instant::now();
        rf.separate(&x, &x, &mut |_, _| true)?;
        println!("roformer alone: {:.1}s for {} chunks", t.elapsed().as_secs_f64(), rf.chunks_for(n));
        return Ok(());
    }
    let kind = if args.get(1).map(|s| s.as_str()) == Some("ort") { BackendKind::Ort } else { BackendKind::CoreMl };
    let dir = clubcraft_core::paths::models_dir();
    let man = Manifest::load(&dir, "htdemucs")?;
    let l = man.segment_samples;
    let mix: Vec<f32> = match args.get(2) {
        Some(p) => {
            let a = npy::read_npy(p.as_ref())?;
            let n = a.shape[1];
            let off = (args.get(3).map(|s| s.parse::<f64>().unwrap()).unwrap_or(0.0) * 44100.0) as usize;
            let mut m = a.data[off..off + l].to_vec();
            m.extend_from_slice(&a.data[n + off..n + off + l]);
            m
        }
        None => npy::read_npy(&dir.join("htdemucs_ref_mix.npy"))?.data,
    };
    let t = std::time::Instant::now();
    let be = backend::load(kind, &man)?;
    println!("{} loaded in {:.2}s", be.name(), t.elapsed().as_secs_f64());
    let s = Spectro::new(man.nfft, man.hop_length);
    let mut spec = vec![0f32; man.spec_len()];
    s.spec(&[&mix[..l], &mix[l..]], &mut spec);
    let mut outs = vec![SourceOut::default(); 4];
    for _ in 0..3 {
        let t = std::time::Instant::now();
        be.run(0, &mix, &spec, &[0, 1, 2, 3], &mut outs)?;
        let nan = outs.iter().map(|o| o.spec.iter().chain(&o.wave).filter(|v| !v.is_finite()).count()).sum::<usize>();
        let rms = (outs[3].wave.iter().map(|v| v * v).sum::<f32>() / (2 * l) as f32).sqrt();
        println!("run {:.2}s, non-finite {nan}, vocals wave rms {rms:.4}", t.elapsed().as_secs_f64());
    }
    Ok(())
}
