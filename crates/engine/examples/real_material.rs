//! Manual check on real music: plays a decoded file through the engine offline at +6 % with
//! keylock on / off and with a +2 semitone key shift, and estimates the pitch change of the
//! long-term spectrum (semitones). Optionally writes the keylocked result for listening.
//!
//! `cargo run --release -p clubcraft-engine --example real_material -- <file.mp3> [out_dir]`
use clubcraft_engine::*;
use rustfft::{num_complex::Complex, FftPlanner};
use std::sync::Arc;

fn log_spectrum(x: &[f32], sr: f64) -> Vec<f64> {
    // average magnitude spectrum, mapped to 1/10-semitone bins from 80 Hz to 4 kHz
    let n = 16384;
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let mut acc = vec![0f64; n / 2];
    let mut buf = vec![Complex::new(0f32, 0f32); n];
    let mut frames = 0;
    let mut i = 0;
    while i + n <= x.len() {
        for k in 0..n {
            let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * k as f32 / n as f32).cos();
            buf[k] = Complex::new(x[i + k] * w, 0.0);
        }
        fft.process(&mut buf);
        for k in 0..n / 2 {
            acc[k] += buf[k].norm() as f64;
        }
        frames += 1;
        i += n / 2;
    }
    let bins = 10 * 12 * 6; // 80 Hz * 2^6 > 4 kHz
    let mut out = vec![0f64; bins];
    for (b, o) in out.iter_mut().enumerate() {
        let f0 = 80.0 * 2f64.powf(b as f64 / 120.0);
        let f1 = 80.0 * 2f64.powf((b + 1) as f64 / 120.0);
        let (k0, k1) = ((f0 / sr * n as f64) as usize, ((f1 / sr * n as f64) as usize).max((f0 / sr * n as f64) as usize + 1));
        let s: f64 = acc[k0..k1].iter().sum::<f64>() / (k1 - k0) as f64 / frames.max(1) as f64;
        *o = (s + 1e-9).ln();
    }
    // whiten (remove the spectral tilt) so the correlation follows harmonics
    let mut w = out.clone();
    for b in 0..bins {
        let lo = b.saturating_sub(60);
        let hi = (b + 60).min(bins);
        w[b] = out[b] - out[lo..hi].iter().sum::<f64>() / (hi - lo) as f64;
    }
    w
}

fn best_shift(a: &[f64], b: &[f64]) -> f64 {
    let mut best = (0i32, f64::MIN);
    for s in -40i32..=40 {
        let mut acc = 0.0;
        for i in 60..a.len() - 60 {
            let j = i as i32 + s;
            acc += a[i] * b[j as usize];
        }
        if acc > best.1 {
            best = (s, acc);
        }
    }
    best.0 as f64 / 10.0
}

fn render(track: &Arc<LoadedTrack>, start: f64, secs: f64, tempo: f64, keylock: bool, shift: f32) -> (Vec<f32>, f64) {
    let mut core = EngineCore::new(44100, 2);
    core.apply(Command::SetLimiter { enabled: false, ceiling_db: 0.0 });
    core.apply(Command::LoadTrack(0, track.clone()));
    core.apply(Command::SetPitchRange(0, PitchRange::Pct10));
    core.apply(Command::SetTempo(0, tempo));
    core.apply(Command::SetKeylock(0, keylock));
    core.apply(Command::SetKeyShift(0, shift));
    core.apply(Command::Seek(0, start));
    core.apply(Command::Play(0));
    let mut m = vec![0f32; 1024 * 2];
    let mut c = vec![0f32; 1024 * 2];
    let mut out = Vec::new();
    let t0 = std::time::Instant::now();
    while out.len() < (secs * 44100.0) as usize {
        core.process(&mut m, &mut c);
        out.extend(m.chunks(2).map(|f| 0.5 * (f[0] + f[1])));
    }
    let speed = secs / t0.elapsed().as_secs_f64();
    (out, speed)
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: real_material <file> [out_dir]");
    let out_dir = std::env::args().nth(2);
    let t0 = std::time::Instant::now();
    let audio = Arc::new(clubcraft_audio::decode_file(path.as_ref(), clubcraft_core::SAMPLE_RATE)?);
    println!("decoded {:.1} s in {:.2} s", audio.duration(), t0.elapsed().as_secs_f64());
    let track = Arc::new(LoadedTrack::new("real", audio.clone()));
    let start = (audio.duration() * 0.4).min(60.0);
    let secs = 20.0;
    // reference: the same musical material, i.e. secs*1.06 seconds of the original
    let s0 = (start * 44100.0) as usize;
    let orig: Vec<f32> = (s0..s0 + (secs * 1.06 * 44100.0) as usize).map(|i| 0.5 * (audio.left[i] + audio.right[i])).collect();
    let ref_spec = log_spectrum(&orig, 44100.0);
    for (name, tempo, keylock, shift, expect) in [
        ("keylock +6 %", 1.06, true, 0.0f32, 0.0),
        ("varispeed +6 %", 1.06, false, 0.0, 12.0 * 1.06f64.log2()),
        ("keylock +6 %, key +2", 1.06, true, 2.0, 2.0),
    ] {
        let (out, speed) = render(&track, start, secs, tempo, keylock, shift);
        let sh = best_shift(&ref_spec, &log_spectrum(&out, 44100.0));
        println!("{name:>22}: measured pitch shift {sh:+.1} semitones (expected {expect:+.2}); rendered at {speed:.0}x realtime");
        if keylock && shift == 0.0 {
            if let Some(dir) = &out_dir {
                let p = std::path::Path::new(dir).join("keylock_106.wav");
                let mut w = wav::WavSink::create(&p, 44100, WavFormat::Pcm16)?;
                let st: Vec<f32> = out.iter().flat_map(|v| [*v, *v]).collect();
                w.write(&st)?;
                w.finish()?;
                println!("wrote {}", p.display());
            }
        }
    }
    Ok(())
}
