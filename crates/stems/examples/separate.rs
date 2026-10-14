//! Separate an audio file into 4 stems (32-bit float WAV).
//!
//! ```text
//! cargo run --release -p clubcraft-stems --example separate -- <audio file> [fast|high|ultra] <out_dir>
//!     [--backend auto|coreml|ort] [--start SECONDS] [--seconds SECONDS] [--shift-samples A,B,..]
//!     [--model htdemucs|htdemucs_ft|htdemucs_6s] [--dump-input] [--cache] [--progressive SECONDS]
//! ```
//! `--dump-input` also writes the decoded (and cropped) input as `input.npy` ([2, N] f32) so the
//! result can be compared with Python Demucs on the identical signal; `--cache` additionally writes
//! and re-reads the FLAC stem cache and reports its size, load time and SNR.

use anyhow::{bail, Context, Result};
use clubcraft_stems::{BackendKind, Quality, Separator};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<()> {
    env_logger_init();
    let mut pos = Vec::new();
    let mut backend = BackendKind::Auto;
    let mut start = 0.0f64;
    let mut seconds: Option<f64> = None;
    let mut dump = false;
    let mut cache = false;
    let mut shift_samples: Option<Vec<usize>> = None;
    let mut model: Option<String> = None;
    let mut progressive: Option<f64> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--backend" => {
                backend = match args.next().as_deref() {
                    Some("coreml") => BackendKind::CoreMl,
                    Some("ort") => BackendKind::Ort,
                    Some("auto") => BackendKind::Auto,
                    x => bail!("bad backend {x:?}"),
                }
            }
            "--start" => start = args.next().context("--start")?.parse()?,
            "--seconds" => seconds = Some(args.next().context("--seconds")?.parse()?),
            "--shift-samples" => {
                shift_samples = Some(
                    args.next().context("--shift-samples")?.split(',').map(|v| v.parse()).collect::<Result<_, _>>()?,
                )
            }
            "--model" => model = Some(args.next().context("--model")?),
            "--dump-input" => dump = true,
            "--progressive" => progressive = Some(args.next().context("--progressive")?.parse()?),
            "--cache" => cache = true,
            _ => pos.push(a),
        }
    }
    let (input, quality, out_dir) = match pos.as_slice() {
        [i, o] => (PathBuf::from(i), Quality::High, PathBuf::from(o)),
        [i, q, o] => (PathBuf::from(i), Quality::parse(q).context("quality must be fast|high|ultra")?, PathBuf::from(o)),
        _ => bail!("usage: separate <audio file> [fast|high|ultra] <out_dir> [--backend auto|coreml|ort]"),
    };
    std::fs::create_dir_all(&out_dir)?;

    let t = Instant::now();
    let mut audio = clubcraft_audio::decode_file(&input, clubcraft_core::SAMPLE_RATE)?;
    let sr = audio.sample_rate as f64;
    let a = ((start * sr) as usize).min(audio.frames());
    let b = seconds.map(|s| (a + (s * sr) as usize).min(audio.frames())).unwrap_or(audio.frames());
    if a > 0 || b < audio.frames() {
        audio.left = audio.left[a..b].to_vec();
        audio.right = audio.right[a..b].to_vec();
    }
    println!("decoded {} ({:.1} s audio) in {:.2} s", input.display(), audio.duration(), t.elapsed().as_secs_f64());
    if dump {
        let mut data = audio.left.clone();
        data.extend_from_slice(&audio.right);
        clubcraft_stems::npy::write_npy(&out_dir.join("input.npy"), &[2, audio.frames()], &data)?;
    }

    let t = Instant::now();
    let mut opts = quality.options();
    opts.backend = backend;
    opts.shift_samples = shift_samples;
    if let Some(m) = model {
        opts.model = m;
    }
    let sep = Separator::with_options(&clubcraft_core::paths::models_dir(), opts)?;
    println!("loaded {} on {} in {:.2} s", sep.model_name(), sep.backend_name(), t.elapsed().as_secs_f64());

    if let Some(at) = progressive {
        // Live-deck latency: time until the stem chunk under `at` seconds is published.
        let target = clubcraft_core::StemTrack::new(audio.frames(), sep.model_name());
        let start = ((at * sr) as usize).min(audio.frames().saturating_sub(1));
        let c0 = start / clubcraft_core::STEM_CHUNK_FRAMES;
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let t = Instant::now();
        let mut first: Option<f64> = None;
        sep.separate_into(&audio, &target, start, &mut |_| {
            if first.is_none() && target.is_chunk_ready(c0) {
                first = Some(t.elapsed().as_secs_f64());
            }
        }, &cancel)?;
        println!(
            "progressive from {at:.1} s: chunk under playhead ready after {:.2} s, complete after {:.2} s",
            first.unwrap_or(f64::NAN),
            t.elapsed().as_secs_f64()
        );
        return Ok(());
    }
    let t = Instant::now();
    let mut last = -1i32;
    let stems = sep.separate(&audio, &mut |p| {
        let pct = (p * 100.0) as i32;
        if pct / 10 != last / 10 {
            last = pct;
            eprint!("\r{pct:3}%");
        }
        true
    })?;
    let dt = t.elapsed().as_secs_f64();
    eprintln!();
    println!(
        "separated {:.1} s of audio in {:.2} s ({:.1}x realtime, {} model runs, {:.3} s/run) [{} / {:?}]",
        audio.duration(),
        dt,
        audio.duration() / dt,
        sep.segments_for(audio.frames()),
        dt / sep.segments_for(audio.frames()) as f64,
        sep.backend_name(),
        quality
    );

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: stems.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    for (kind, (l, r)) in clubcraft_core::StemKind::ALL.iter().zip(&stems.stems) {
        let name = match kind {
            clubcraft_core::StemKind::Drums => "drums",
            clubcraft_core::StemKind::Bass => "bass",
            clubcraft_core::StemKind::Other => "other",
            clubcraft_core::StemKind::Vocals => "vocals",
        };
        let mut w = hound::WavWriter::create(out_dir.join(format!("{name}.wav")), spec)?;
        for (a, b) in l.iter().zip(r) {
            w.write_sample(*a)?;
            w.write_sample(*b)?;
        }
        w.finalize()?;
    }
    println!("wrote stems to {}", out_dir.display());

    if cache {
        let p = out_dir.join("cache.stems.flac");
        let t = Instant::now();
        clubcraft_stems::save_stems_with_model(&p, &stems, sep.model_name())?;
        let enc = t.elapsed().as_secs_f64();
        let size = std::fs::metadata(&p)?.len();
        let t = Instant::now();
        let back = clubcraft_stems::load_stems(&p)?;
        let dec = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let track = clubcraft_stems::load_stem_track(&p)?;
        let dec2 = t.elapsed().as_secs_f64();
        assert!(track.is_complete());
        let mut worst = f64::INFINITY;
        for (x, y) in stems.stems.iter().zip(&back.stems) {
            for (u, v) in [(&x.0, &y.0), (&x.1, &y.1)] {
                let num: f64 = u.iter().map(|a| (*a as f64).powi(2)).sum();
                let den: f64 = u.iter().zip(v.iter()).map(|(a, b)| (*a as f64 - *b as f64).powi(2)).sum();
                worst = worst.min(10.0 * (num / den.max(1e-30)).log10());
            }
        }
        println!(
            "cache: {:.1} MB ({:.0} kbit/s), save {:.2} s, load_stems {:.2} s, load_stem_track {:.2} s, worst SNR {:.1} dB",
            size as f64 / 1e6,
            size as f64 * 8.0 / 1000.0 / audio.duration(),
            enc,
            dec,
            dec2,
            worst
        );
    }
    Ok(())
}

fn env_logger_init() {
    // Keep the example dependency-free: print warnings from the `log` crate to stderr.
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) && r.target().starts_with("clubcraft") {
                eprintln!("[{}] {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: L = L;
    let _ = log::set_logger(&LOGGER).map(|_| log::set_max_level(log::LevelFilter::Info));
}
