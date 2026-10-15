//! Analyse one track and print grid, key, loudness, sections and cues.
//!
//! `cargo run --release -p clubcraft-analysis --example analyze -- <file> [--stems <dir>]`
//!
//! `--stems <dir>`: directory with drums/bass/other/vocals.{wav,mp3,flac} (e.g. Demucs output).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine

#[path = "common/mod.rs"]
mod common;

use clubcraft_analysis::{analyze, waveform, AnalyzeOptions};
use common::fmt_time;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let mut args = std::env::args().skip(1);
    let mut file = None;
    let mut stems_dir = None;
    let mut show_bars = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--stems" => stems_dir = args.next().map(PathBuf::from),
            "--bars" => show_bars = true,
            _ => file = Some(PathBuf::from(a)),
        }
    }
    let file = file.ok_or_else(|| anyhow::anyhow!("usage: analyze <file> [--stems <dir>]"))?;
    let t = Instant::now();
    let audio = clubcraft_audio::decode_file(&file, 44_100)?;
    println!(
        "{}: {:.1} s, decoded in {:.2?}",
        file.display(),
        audio.duration(),
        t.elapsed()
    );
    let stems = match &stems_dir {
        Some(d) => Some(common::load_stems(d, &audio)?),
        None => None,
    };
    let sref = stems.as_ref().map(|s| s.as_ref());
    let t = Instant::now();
    let a = analyze(&audio, sref.as_ref(), &AnalyzeOptions::default());
    let ta = t.elapsed();
    let t = Instant::now();
    let wf = waveform(&audio, sref);
    let tw = t.elapsed();
    println!(
        "analysis {:.2?}, waveform {:.2?} ({} bins @ {:.2}/s){}",
        ta,
        tw,
        wf.len(),
        wf.rate,
        if stems.is_some() { " [stems]" } else { "" }
    );
    match a.grid {
        Some(g) => println!("grid: {:.3} BPM, downbeat {:.4} s", g.bpm, g.downbeat),
        None => println!("grid: none"),
    }
    if let Some(k) = a.key {
        println!("key: {} ({})", k.name(), k.camelot());
    }
    println!(
        "loudness: {:.2} LUFS, gain {:+.2} dB, energy {}",
        a.loudness_lufs.unwrap_or(f32::NAN),
        a.gain_db.unwrap_or(f32::NAN),
        a.energy.unwrap_or(0)
    );
    if let (true, Some(g)) = (show_bars, a.grid) {
        let b = clubcraft_analysis::structure::bar_features_for(&audio, sref, &g);
        println!(
            "\n{:>5} {:>9} {:>6} {:>6} {:>6} {:>6} {:>6} {:>5}  drm  bas  mel  vox  energy",
            "bar", "time", "level", "low", "lmid", "mid", "high", "dens"
        );
        for i in 0..b.len() {
            let s = b.stems[i];
            println!(
                "{:>5} {:>9} {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>6.1} {:>5.2}  {:.2} {:.2} {:.2} {:.2}  {:.2}",
                b.bar_index(i),
                fmt_time(b.start[i].max(0.0)),
                b.level_db[i],
                b.bands_db[i][0],
                b.bands_db[i][1],
                b.bands_db[i][2],
                b.bands_db[i][3],
                b.density[i],
                s[0],
                s[1],
                s[2],
                s[3],
                b.energy[i]
            );
        }
    }
    println!(
        "\n{:<7} {:>10} {:>10} {:>6} {:>5} {:>6}  drums bass  mel  vox",
        "section", "start", "end", "bar", "bars", "energy"
    );
    for s in &a.sections {
        println!(
            "{:<7} {:>10} {:>10} {:>6} {:>5} {:>6.2}  {:>5.2} {:>4.2} {:>4.2} {:>4.2}",
            s.kind.label(),
            fmt_time(s.start),
            fmt_time(s.end),
            s.start_bar,
            s.bars,
            s.energy,
            s.stems[0],
            s.stems[1],
            s.stems[2],
            s.stems[3]
        );
    }
    println!(
        "\n{:<5} {:<8} {:>10} {:>7} {:>6}",
        "slot", "cue", "time", "beat", "bar"
    );
    for c in &a.cues {
        let beat = a.grid.map(|g| g.beat_at(c.pos)).unwrap_or(0.0);
        println!(
            "{:<5} {:<8} {:>10} {:>7.1} {:>6.1}",
            c.slot.map(|s| s.to_string()).unwrap_or("-".into()),
            c.name,
            fmt_time(c.pos),
            beat,
            beat / 4.0
        );
    }
    Ok(())
}
