//! Progressive separation (`separate_into`) must publish exactly what `separate` computes.

use clubcraft_audio::AudioBuffer;
use clubcraft_core::{StemTrack, STEM_CHANNELS, STEM_CHUNK_FRAMES};
use clubcraft_stems::{Options, Quality, Separator};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn models() -> Option<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models");
    d.join("htdemucs.json").exists().then_some(d)
}

/// ~31 s of a synthetic "track": kick-like pulses, a bass line, a chord pad and noise hats.
fn test_audio(frames: usize) -> AudioBuffer {
    let sr = 44100.0f32;
    let mut l = Vec::with_capacity(frames);
    let mut r = Vec::with_capacity(frames);
    let mut seed = 12345u32;
    for i in 0..frames {
        let t = i as f32 / sr;
        let beat = (t * 2.0).fract();
        let kick = (-beat * 30.0).exp() * (2.0 * std::f32::consts::PI * (50.0 + 80.0 * (-beat * 40.0).exp()) * beat).sin();
        let bass = 0.3 * (2.0 * std::f32::consts::PI * 55.0 * t).sin() * if (t * 4.0) as i32 % 2 == 0 { 1.0 } else { 0.5 };
        let pad = 0.1 * [261.6f32, 329.6, 392.0].iter().map(|f| (2.0 * std::f32::consts::PI * f * t).sin()).sum::<f32>();
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = ((seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 0.2 * (-((t * 4.0).fract()) * 50.0).exp();
        l.push(0.5 * kick + bass + pad + noise);
        r.push(0.5 * kick + bass + 0.8 * pad - noise);
    }
    AudioBuffer::new(44100, l, r)
}

fn check(opts: Options, frames: usize, start: usize, tol: f32) {
    let Some(dir) = models() else { return };
    let sep = Separator::with_options(&dir, opts).unwrap();
    let audio = test_audio(frames);
    let full = sep.separate(&audio, &mut |_| true).unwrap();
    assert_eq!(full.frames(), frames);
    let target = StemTrack::new(frames, sep.model_name());
    let cancel = AtomicBool::new(false);
    let mut first_ready = None;
    let mut calls = 0;
    sep.separate_into(&audio, &target, start, &mut |_| {
        calls += 1;
        if first_ready.is_none() && target.is_chunk_ready(start / STEM_CHUNK_FRAMES) {
            first_ready = Some(calls);
        }
    }, &cancel)
    .unwrap();
    assert!(target.is_complete());
    let mut max_diff = 0f32;
    let mut per = [0f32; 4];
    let mut worst_frame = [0usize; 4];
    for c in 0..target.num_chunks() {
        let data = &target.chunk(c).unwrap().data;
        for (k, f) in data.chunks_exact(STEM_CHANNELS).enumerate() {
            let i = c * STEM_CHUNK_FRAMES + k;
            for s in 0..4 {
                let d = (f[2 * s] - full.stems[s].0[i]).abs().max((f[2 * s + 1] - full.stems[s].1[i]).abs());
                if d > per[s] {
                    per[s] = d;
                    worst_frame[s] = i;
                }
                max_diff = max_diff.max(d);
            }
        }
    }
    println!("per stem max diff {per:?} at frames {worst_frame:?}");
    println!(
        "{} on {}: max |progressive - full| = {max_diff:e}; start chunk ready after {first_ready:?} of {calls} model runs",
        sep.model_name(),
        sep.backend_name()
    );
    assert!(max_diff <= tol, "max diff {max_diff}");
}

#[test]
fn progressive_equals_full_fast() {
    // One model, one shift, overlap 0.25: every sample has at most two contributions, so the
    // result is bit-identical regardless of processing order.
    check(Quality::Fast.options(), 31 * 44100 + 777, 20 * 44100, 0.0);
}

#[test]
fn progressive_equals_full_high() {
    // Bag of 4 models, each contributing to one stem only: still bit-identical.
    check(Quality::High.options(), 20 * 44100 + 3, 9 * 44100, 0.0);
}

#[test]
fn progressive_equals_full_best() {
    // RoFormer vocals (computed lazily in window order) + htdemucs_ft drums/bass + residual other.
    let Some(dir) = models() else { return };
    if !dir.join("melroformer.json").exists() {
        return;
    }
    check(Quality::Best.options(), 26 * 44100 + 11, 14 * 44100, 0.0);
}

#[test]
fn progressive_equals_full_shifts_overlap() {
    let mut o = Quality::Fast.options();
    o.shifts = 2;
    o.overlap = 0.5;
    check(o, 25 * 44100 + 5, 12 * 44100, 1e-6);
}

#[test]
fn cancel_stops() {
    let Some(dir) = models() else { return };
    let sep = Separator::with_options(&dir, Quality::Fast.options()).unwrap();
    let audio = test_audio(40 * 44100);
    let target = StemTrack::new(audio.frames(), "htdemucs");
    let cancel = AtomicBool::new(false);
    let err = sep
        .separate_into(&audio, &target, 0, &mut |_| cancel.store(true, std::sync::atomic::Ordering::Relaxed), &cancel)
        .unwrap_err();
    assert!(err.is::<clubcraft_stems::Cancelled>());
    assert!(!target.is_complete());
}

#[test]
#[ignore = "slow: two full Best runs"]
fn best_is_deterministic() {
    let Some(dir) = models() else { return };
    let sep = Separator::with_options(&dir, Quality::Best.options()).unwrap();
    let audio = test_audio(26 * 44100 + 11);
    let a = sep.separate(&audio, &mut |_| true).unwrap();
    let b = sep.separate(&audio, &mut |_| true).unwrap();
    for s in 0..4 {
        let d = a.stems[s].0.iter().zip(&b.stems[s].0).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
        println!("stem {s}: run-to-run max diff {d:e}");
    }
}
