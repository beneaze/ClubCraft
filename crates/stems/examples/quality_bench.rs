//! "Remix" quality benchmark without ground-truth multitracks: builds a mixture from stems of
//! four *different* tracks (drums of A + bass of B + other of C + vocals of D, each a directory with
//! `drums.wav` etc. written by the `separate` example), separates it with every preset and prints
//! the SDR of each estimated stem against the stem that went into the mix.
//!
//! ```text
//! cargo run --release -p clubcraft-stems --example quality_bench -- DIR_A DIR_B DIR_C DIR_D [--backend coreml|ort]
//!     [--presets fast,high,ultra,best,best-mix,best-ov4]
//! ```
//! Caveat: the reference stems are themselves model outputs, so absolute numbers are optimistic;
//! the comparison between presets is what matters.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine

use anyhow::{bail, Context, Result};
use clubcraft_audio::AudioBuffer;
use clubcraft_stems::{BackendKind, DemucsInput, Options, Quality, Separator};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn read_wav(p: &Path) -> Result<(Vec<f32>, Vec<f32>)> {
    let mut r = hound::WavReader::open(p).with_context(|| format!("open {}", p.display()))?;
    let s: Vec<f32> = r.samples::<f32>().collect::<Result<_, _>>()?;
    Ok((s.iter().step_by(2).copied().collect(), s.iter().skip(1).step_by(2).copied().collect()))
}

fn sdr(r: &[f32], e: &[f32]) -> f64 {
    let num: f64 = r.iter().map(|v| (*v as f64).powi(2)).sum();
    let den: f64 = r.iter().zip(e).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum();
    10.0 * (num / den.max(1e-20)).log10()
}

fn main() -> Result<()> {
    let mut dirs = Vec::new();
    let mut backend = BackendKind::Auto;
    let mut presets = "fast,high,ultra,best".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--presets" {
            presets = args.next().unwrap_or_default();
        } else if a == "--backend" {
            backend = match args.next().as_deref() {
                Some("coreml") => BackendKind::CoreMl,
                Some("ort") => BackendKind::Ort,
                _ => BackendKind::Auto,
            };
        } else {
            dirs.push(PathBuf::from(a));
        }
    }
    if dirs.len() != 4 {
        bail!("usage: quality_bench DIR_DRUMS DIR_BASS DIR_OTHER DIR_VOCALS");
    }
    let names = ["drums", "bass", "other", "vocals"];
    let mut refs = Vec::new();
    for (d, n) in dirs.iter().zip(names) {
        refs.push(read_wav(&d.join(format!("{n}.wav")))?);
    }
    let len = refs.iter().map(|r| r.0.len()).min().unwrap();
    let mut l = vec![0f32; len];
    let mut r = vec![0f32; len];
    for (a, b) in &refs {
        for i in 0..len {
            l[i] += a[i];
            r[i] += b[i];
        }
    }
    let mix = AudioBuffer::new(44100, l, r);
    println!("mixture: {:.1} s", mix.duration());
    let instr_ref: Vec<(Vec<f32>, Vec<f32>)> = vec![(
        (0..len).map(|i| refs[0].0[i] + refs[1].0[i] + refs[2].0[i]).collect(),
        (0..len).map(|i| refs[0].1[i] + refs[1].1[i] + refs[2].1[i]).collect(),
    )];
    println!(
        "{:9} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9}",
        "preset", "drums", "bass", "other", "vocals", "instr", "mean4", "time"
    );
    for p in presets.split(',') {
        let mut o = match p {
            "best-mix" => Options { demucs_input: DemucsInput::Mix, ..Quality::Best.options() },
            "best-ov4" => Options { vocals_overlap: Some(4), ..Quality::Best.options() },
            q => Quality::parse(q).context("preset")?.options(),
        };
        o.backend = backend;
        let sep = Separator::with_options(&clubcraft_core::paths::models_dir(), o)?;
        let t = Instant::now();
        let st = sep.separate(&mix, &mut |_| true)?;
        let dt = t.elapsed().as_secs_f64();
        let mut row = Vec::new();
        for (k, (rl, rr)) in refs.iter().enumerate() {
            let (el, er) = &st.stems[k];
            row.push(0.5 * (sdr(&rl[..len], el) + sdr(&rr[..len], er)));
        }
        let mean = row.iter().sum::<f64>() / 4.0;
        let il: Vec<f32> = (0..len).map(|i| st.stems[0].0[i] + st.stems[1].0[i] + st.stems[2].0[i]).collect();
        let ir: Vec<f32> = (0..len).map(|i| st.stems[0].1[i] + st.stems[1].1[i] + st.stems[2].1[i]).collect();
        let instr = 0.5 * (sdr(&instr_ref[0].0, &il) + sdr(&instr_ref[0].1, &ir));
        println!(
            "{:9} {:8.2} {:8.2} {:8.2} {:8.2} {:8.2} {:8.2} {:8.1}s",
            p, row[0], row[1], row[2], row[3], instr, mean, dt
        );
    }
    Ok(())
}
