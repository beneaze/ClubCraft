//! COCOLA front end and embeddings against the PyTorch / librosa reference written by
//! `tools/model-export/export_cocola.py` (skipped when the models are not exported).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // tests fail by panicking

use clubcraft_analysis::cocola;

fn npy(name: &str) -> Option<clubcraft_stems::npy::NpyArray> {
    let p = clubcraft_core::paths::models_dir().join(name);
    p.exists().then(|| clubcraft_stems::npy::read_npy(&p).unwrap())
}

#[test]
fn front_end_and_embeddings_match_reference() {
    let (Some(audio), Some(feats)) = (npy("cocola_ref_audio.npy"), npy("cocola_ref_features.npy")) else {
        eprintln!("cocola reference not found, skipping");
        return;
    };
    let n = audio.shape[0];
    let per = 2 * cocola::N_MELS * cocola::FRAMES;
    let mut ours = Vec::new();
    for i in 0..n {
        let f = cocola::features(&audio.data[i * cocola::CLIP..(i + 1) * cocola::CLIP]);
        let r = &feats.data[i * per..(i + 1) * per];
        let max = f.iter().zip(r).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        let mean = f.iter().zip(r).map(|(a, b)| (a - b).abs()).sum::<f32>() / per as f32;
        println!("clip {i}: max |Δ| {max:.4} dB, mean {mean:.5} dB");
        assert!(mean < 0.01 && max < 0.5, "features differ: max {max} mean {mean}");
        ours.push(f);
    }
    #[cfg(feature = "allinone")]
    if cocola::Cocola::available() {
        let m = cocola::Cocola::load_default().unwrap();
        let e = m.embed(&ours, false).unwrap();
        let r = npy("cocola_ref_embeddings.npy").unwrap();
        let max = e.iter().flatten().zip(&r.data).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        println!("embeddings max |Δ| {max:.5}");
        assert!(max < 2e-3);
    }
}
