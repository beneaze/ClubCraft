//! Evaluate the analysis against DJ.Studio's (and Mixed In Key's) analysis of the same tracks.
//!
//! `cargo run --release -p clubcraft-analysis --example eval -- [options]`
//!
//! Reads (read-only) `~/Music/DJ.Studio/Database/audio-library-table` and
//! `track-structures-table`, finds the audio files locally (the recorded path, or by file name
//! under ~/Music, ~/Downloads, ~/Desktop/New_Selected_mix), analyses each one and reports:
//! BPM agreement, beat phase offset (ms), bar phase (downbeat) agreement, key agreement
//! (exact / relative / fifth), MIK cue and MIK energy-segment agreement, energy vs MIK energy.
//!
//! Options:
//! * `--db <dir>`          DJ.Studio Database dir (default ~/Music/DJ.Studio/Database)
//! * `--stems-root <dir>`  directory with `<file stem>/{drums,bass,other,vocals}.*` (Demucs
//!   layout) to also evaluate with stems
//! * `--only-stems`        only evaluate tracks that have stems
//! * `--filter <text>`     only tracks whose file name contains the text
//! * `--limit <n>`         at most n tracks
//! * `--csv <file>`        write per-track results as CSV
//! * `--only-mik`          only tracks with Mixed In Key cue points
//! * `--make-stems`        separate missing stems with clubcraft-stems (`Quality::Fast`) into
//!   `--stems-root` as `<file stem>.stems.flac` (one track at a time)
//! * `--act-cache <dir>`   cache All-In-One activations (`<file stem>.act.npy`); with stems the
//!   structure is evaluated with rules and with the model
//! * `--jobs <n>`          tracks analysed in parallel (default 4; use 1-2 with stems / model)
//! * `-v`                  per-track detail (cues; `*` = slot-less marker)
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine

#[path = "common/mod.rs"]
mod common;

use clubcraft_analysis::{analyze, AnalyzeOptions};
use clubcraft_core::{Analysis, CueKind, Key};
use rayon::prelude::*;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Debug, Clone)]
struct Truth {
    name: String,
    path: PathBuf,
    /// DJ.Studio fixed grid: tempo (fit over beatData) and time of beat nr 0.
    bpm: f64,
    beat0: f64,
    /// DJ.Studio's beat data is not a constant grid (manual / variable edits; residual > 20 ms).
    variable: bool,
    key: Option<Key>,
    mik_key: Option<Key>,
    /// Mixed In Key cue points (beat numbers relative to beat 0).
    mik_cues: Vec<i64>,
    /// MIK energy segments start beats.
    mik_segments: Vec<i64>,
    mik_energy: Option<u8>,
    system_cues: Vec<i64>,
    auto_gain: Option<f64>,
}

/// DJ.Studio's integer key numbers (`camelotKey`, `noteKey`, `mikKey`) → keys: 0..=11 are the
/// major keys by pitch class (0 = C), 12..=23 the relative minor of major key `n - 12`
/// (12 = A minor, 22 = G minor); −1 = unknown. Derived from records that also carry a key tag and
/// from ClubCraft's own analysis of the same files (`docs/interop/djstudio.md`, §2.3).
fn camelot_int_to_key(v: i64) -> Option<Key> {
    match v {
        0..=11 => Some(Key::new(v as u8, false)),
        12..=23 => Some(Key::new(((v - 12 + 9) % 12) as u8, true)),
        _ => None,
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn index_audio(roots: &[PathBuf]) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut stack: Vec<PathBuf> = roots.to_vec();
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if p.is_dir() {
                if name == "DJ.Studio"
                    || name == "ClubCraft"
                    || name.ends_with(".app")
                    || name == "PioneerDJ"
                {
                    continue;
                }
                stack.push(p);
            } else {
                let lower = name.to_lowercase();
                if [".mp3", ".wav", ".flac", ".m4a", ".aiff", ".aif"]
                    .iter()
                    .any(|x| lower.ends_with(x))
                {
                    out.entry(name).or_insert(p);
                }
            }
        }
    }
    out
}

fn fit_line(pts: &[(f64, f64)]) -> Option<(f64, f64)> {
    let n = pts.len() as f64;
    if n < 2.0 {
        return None;
    }
    let (sx, sy) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    let (mx, my) = (sx / n, sy / n);
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for p in pts {
        sxx += (p.0 - mx) * (p.0 - mx);
        sxy += (p.0 - mx) * (p.1 - my);
    }
    let slope = sxy / sxx;
    Some((my - slope * mx, slope))
}

fn load_truth(db: &Path, index: &HashMap<String, PathBuf>) -> Vec<Truth> {
    let mut structures: HashMap<String, Value> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(db.join("track-structures-table")) {
        for b in rd.flatten() {
            let Ok(rd2) = std::fs::read_dir(b.path()) else {
                continue;
            };
            for f in rd2.flatten() {
                let p = f.path();
                if p.is_file() {
                    if let Ok(Ok(v)) =
                        std::fs::read(&p).map(|b| serde_json::from_slice::<Value>(&b))
                    {
                        if let Some(k) = v["key"].as_str() {
                            structures.insert(k.to_string(), v);
                        }
                    }
                } else if p.is_dir() {
                    let q = p.join("fixed");
                    if let Ok(Ok(v)) =
                        std::fs::read(&q).map(|b| serde_json::from_slice::<Value>(&b))
                    {
                        if let Some(k) = v["key"].as_str() {
                            structures.insert(k.to_string(), v);
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut seen_hash = HashSet::new();
    let Ok(rd) = std::fs::read_dir(db.join("audio-library-table")) else {
        return out;
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for b in rd.flatten() {
        if let Ok(rd2) = std::fs::read_dir(b.path()) {
            files.extend(rd2.flatten().map(|f| f.path()).filter(|p| p.is_file()));
        }
    }
    files.sort();
    for f in files {
        let Ok(Ok(rec)) = std::fs::read(&f).map(|b| serde_json::from_slice::<Value>(&b)) else {
            continue;
        };
        let name = rec["name"].as_str().unwrap_or("").to_string();
        let loc = rec["externalRec"]["fileLocationPath"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let mut path = PathBuf::from(&loc);
        if !path.is_file() {
            let base = Path::new(&loc)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            match index.get(&base).or_else(|| index.get(&name)) {
                Some(p) => path = p.clone(),
                None => continue,
            }
        }
        let hash = rec["fileHash"].as_str().unwrap_or("").to_string();
        if !seen_paths.insert(path.clone()) || (!hash.is_empty() && !seen_hash.insert(hash)) {
            continue;
        }
        let skey = rec["structureKey"].as_str().unwrap_or("").to_string();
        let st = structures
            .get(&skey)
            .or_else(|| structures.get(&format!("{skey}/fixed")));
        let Some(st) = st else { continue };
        let Some(bd) = st["beatData"].as_array() else {
            continue;
        };
        let pts: Vec<(f64, f64)> = bd
            .iter()
            .filter_map(|b| Some((b["nr"].as_f64()?, b["time"].as_f64()?)))
            .collect();
        let Some((beat0, spb)) = fit_line(&pts) else {
            continue;
        };
        let resid = (pts
            .iter()
            .map(|(n, t)| (t - (beat0 + spb * n)).powi(2))
            .sum::<f64>()
            / pts.len() as f64)
            .sqrt();
        if spb <= 0.0 {
            continue;
        }
        let mik_key = rec["mikKey"].as_i64().and_then(camelot_int_to_key);
        let key = rec["camelotKey"].as_i64().and_then(camelot_int_to_key);
        let cue_beats = |arr: &Value, ty: Option<i64>| -> Vec<i64> {
            let mut v: Vec<i64> = arr
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|c| ty.is_none_or(|t| c["type"].as_i64() == Some(t)))
                        .filter_map(|c| c["beat"].as_i64())
                        .filter(|b| *b >= 0)
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v.dedup();
            v
        };
        let mik_cues = cue_beats(&rec["cueData"]["hotCuePoints"], Some(2));
        let system_cues = rec["cueData"]["systemCuePoints"]
            .as_array()
            .map(|a| a.iter().filter_map(|c| c["beat"].as_i64()).collect())
            .unwrap_or_default();
        let main_st = structures.get(&skey);
        let mik_segments: Vec<i64> = main_st
            .and_then(|s| s["energyLevelData"].as_array())
            .map(|a| a.iter().filter_map(|e| e["startBeatNr"].as_i64()).collect())
            .unwrap_or_default();
        let mik_energy = rec["mikEnergy"]
            .as_i64()
            .filter(|e| *e > 0)
            .map(|e| e as u8);
        out.push(Truth {
            name: path.file_name().unwrap().to_string_lossy().to_string(),
            path,
            bpm: 60.0 / spb,
            beat0,
            variable: resid > 0.02,
            key,
            mik_key,
            mik_cues,
            mik_segments,
            mik_energy,
            system_cues,
            auto_gain: rec["autoGain"].as_f64(),
        });
    }
    out
}

#[derive(Debug, Clone, Default)]
struct Res {
    name: String,
    gt_variable: bool,
    stems: bool,
    secs: f64,
    bpm: f64,
    bpm_gt: f64,
    bpm_ok: bool,
    /// Median signed beat phase offset (ours - GT, ms) over the track.
    phase_ms: f64,
    /// Bar phase: (our downbeat - GT beat0) in beats mod 4, or -1 if beat phase is off.
    bar_off: i64,
    /// MIK cue beats mod 4 agreement with our bars (fraction).
    mik_bar: Option<f64>,
    /// Bar phase agrees with the reference (MIK cue majority if present, else DJ.Studio beat 0).
    bar_ref_ok: Option<bool>,
    key: String,
    key_gt: String,
    key_rel: &'static str,
    mik_recall: Option<f64>,
    seg_recall: Option<f64>,
    seg_precision: Option<f64>,
    /// Raw boundaries (before labelling / merging): (MIK segment recall, precision, MIK cue recall).
    raw: Option<(f64, f64, f64)>,
    energy: u8,
    mik_energy: Option<u8>,
    lufs: f32,
    dj_target: Option<f64>,
    mix_in_bars: Option<f64>,
    mix_out_bars: Option<f64>,
    detail: String,
}

fn key_relation(a: Key, b: Key) -> &'static str {
    if a == b {
        return "exact";
    }
    if a.camelot_number() == b.camelot_number() {
        return "relative";
    }
    let (x, y) = (a.camelot_number() as i32, b.camelot_number() as i32);
    let d = (x - y).rem_euclid(12);
    if a.minor == b.minor && (d == 1 || d == 11) {
        return "fifth";
    }
    if a.tonic == b.tonic {
        return "parallel";
    }
    "other"
}

fn evaluate(t: &Truth, a: &Analysis, stems: bool, secs: f64, verbose: bool) -> Res {
    let mut r = Res {
        name: t.name.clone(),
        stems,
        secs,
        bpm_gt: t.bpm,
        gt_variable: t.variable,
        ..Default::default()
    };
    let gt_spb = 60.0 / t.bpm;
    if let Some(g) = a.grid {
        r.bpm = g.bpm;
        r.bpm_ok = (g.bpm - t.bpm).abs() < 0.05;
        // Phase offset at 9 points over the track (ours - nearest GT beat).
        let mut offs: Vec<f64> = (1..10)
            .map(|i| {
                let tt = a.duration * i as f64 / 10.0;
                let ours = g.time_of_beat(g.beat_at(tt).round());
                let k = ((ours - t.beat0) / gt_spb).round();
                ours - (t.beat0 + k * gt_spb)
            })
            .collect();
        offs.sort_by(|x, y| x.total_cmp(y));
        r.phase_ms = offs[4] * 1000.0;
        let beats = (g.downbeat - t.beat0) / gt_spb;
        r.bar_off = if (beats - beats.round()).abs() < 0.25 {
            (beats.round() as i64).rem_euclid(4)
        } else {
            -1
        };
        if r.bar_off >= 0 {
            let mut votes = [0usize; 4];
            for b in &t.mik_cues {
                votes[(*b).rem_euclid(4) as usize] += 1;
            }
            let shift = if t.mik_cues.is_empty() {
                0
            } else {
                (0..4).max_by_key(|i| votes[*i]).unwrap() as i64
            };
            r.bar_ref_ok = Some((r.bar_off - shift).rem_euclid(4) == 0);
        }
        if !t.mik_cues.is_empty() {
            let ok = t
                .mik_cues
                .iter()
                .filter(|&&b| {
                    let tt = t.beat0 + b as f64 * gt_spb;
                    let ob = g.beat_at(tt);
                    (ob - ob.round()).abs() < 0.25 && (ob.round() as i64).rem_euclid(4) == 0
                })
                .count();
            r.mik_bar = Some(ok as f64 / t.mik_cues.len() as f64);
            // Cue recall: MIK cues (excluding beat 0/1) matched by one of our cues within 1 bar.
            let ours: Vec<f64> = a.cues.iter().map(|c| c.pos).collect();
            let mk: Vec<i64> = t.mik_cues.iter().copied().filter(|b| *b > 4).collect();
            if !mk.is_empty() {
                let hit = mk
                    .iter()
                    .filter(|&&b| {
                        let tt = t.beat0 + b as f64 * gt_spb;
                        ours.iter().any(|o| (o - tt).abs() < 4.0 * gt_spb * 1.01)
                    })
                    .count();
                r.mik_recall = Some(hit as f64 / mk.len() as f64);
            }
        }
        if t.mik_segments.len() > 1 {
            let bounds: Vec<f64> = a.sections.iter().skip(1).map(|s| s.start).collect();
            let segs: Vec<i64> = t.mik_segments.iter().copied().filter(|b| *b > 0).collect();
            let hit = segs
                .iter()
                .filter(|&&b| {
                    let tt = t.beat0 + b as f64 * gt_spb;
                    bounds.iter().any(|o| (o - tt).abs() < 4.0 * gt_spb * 1.01)
                })
                .count();
            if !segs.is_empty() {
                r.seg_recall = Some(hit as f64 / segs.len() as f64);
            }
            let last = t.mik_segments.iter().copied().max().unwrap_or(0) as f64;
            let ours: Vec<f64> = bounds
                .iter()
                .copied()
                .filter(|o| (o - t.beat0) / gt_spb <= last + 4.0)
                .collect();
            if !ours.is_empty() {
                let ph = ours
                    .iter()
                    .filter(|o| {
                        segs.iter().any(|&b| {
                            (t.beat0 + b as f64 * gt_spb - **o).abs() < 4.0 * gt_spb * 1.01
                        })
                    })
                    .count();
                r.seg_precision = Some(ph as f64 / ours.len() as f64);
            }
        }
        let cue_bars = |k: CueKind| a.cue(k).map(|c| g.beat_at(c.pos) / 4.0);
        if t.system_cues.len() >= 2 {
            let gt_bar = |b: i64| (t.beat0 + b as f64 * gt_spb - g.downbeat) / g.bar_len();
            r.mix_in_bars = cue_bars(CueKind::MixIn).map(|x| x - gt_bar(t.system_cues[0]));
            r.mix_out_bars = cue_bars(CueKind::MixOut).map(|x| x - gt_bar(t.system_cues[1]));
        }
        if verbose {
            let mut d = String::new();
            for c in &a.cues {
                d += &format!(
                    " {}{}@{:.0}",
                    if c.slot.is_some() { "" } else { "*" },
                    c.name.replace(' ', ""),
                    g.beat_at(c.pos) + (g.downbeat - t.beat0) / gt_spb
                );
            }
            d += " | MIK:";
            for b in &t.mik_cues {
                d += &format!(" {b}");
            }
            d += " | segs:";
            for s in &a.sections {
                d += &format!(
                    " {}@{}",
                    s.kind.label(),
                    (g.beat_at(s.start) + (g.downbeat - t.beat0) / gt_spb).round()
                );
            }
            d += " | MIKseg:";
            for b in &t.mik_segments {
                d += &format!(" {b}");
            }
            r.detail = d;
        }
    }
    let gt_key = t.mik_key.or(t.key);
    if let (Some(k), Some(gk)) = (a.key, gt_key) {
        r.key = k.camelot();
        r.key_gt = gk.camelot();
        r.key_rel = key_relation(k, gk);
    } else {
        r.key_rel = "none";
    }
    r.energy = a.energy.unwrap_or(0);
    r.mik_energy = t.mik_energy;
    r.lufs = a.loudness_lufs.unwrap_or(f32::NAN);
    r.dj_target = t.auto_gain.map(|g| r.lufs as f64 + 20.0 * g.log10());
    r
}

/// Recall / precision of boundary times against MIK segment starts, and MIK cue recall.
fn raw_scores(t: &Truth, bounds: &[f64]) -> Option<(f64, f64, f64)> {
    let spb = 60.0 / t.bpm;
    let tol = 4.0 * spb * 1.01;
    let near = |x: f64, set: &[f64]| set.iter().any(|y| (x - y).abs() < tol);
    let segs: Vec<f64> = t
        .mik_segments
        .iter()
        .filter(|b| **b > 0)
        .map(|b| t.beat0 + *b as f64 * spb)
        .collect();
    let cues: Vec<f64> = t
        .mik_cues
        .iter()
        .filter(|b| **b > 4)
        .map(|b| t.beat0 + *b as f64 * spb)
        .collect();
    if segs.is_empty() || cues.is_empty() {
        return None;
    }
    let last = segs.iter().cloned().fold(0.0, f64::max) + tol;
    let ours: Vec<f64> = bounds
        .iter()
        .copied()
        .filter(|b| *b > 1.0 && *b <= last)
        .collect();
    let rec = segs.iter().filter(|s| near(**s, bounds)).count() as f64 / segs.len() as f64;
    let prec = if ours.is_empty() {
        0.0
    } else {
        ours.iter().filter(|o| near(**o, &segs)).count() as f64 / ours.len() as f64
    };
    let crec = cues.iter().filter(|c| near(**c, bounds)).count() as f64 / cues.len() as f64;
    Some((rec, prec, crec))
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "-".into()
    } else {
        format!("{:5.1}% ({n}/{d})", 100.0 * n as f64 / d as f64)
    }
}

fn summarize(label: &str, all: &[Res]) {
    let n_var = all.iter().filter(|r| r.gt_variable).count();
    let grid_rs: Vec<Res> = all.iter().filter(|r| !r.gt_variable).cloned().collect();
    println!("\n=== {label}: {} tracks ({n_var} with a non-constant DJ.Studio grid excluded from grid metrics) ===", all.len());
    let rs = &grid_rs[..];
    let n = rs.len();
    let bpm_ok = rs.iter().filter(|r| r.bpm_ok).count();
    let octave = rs
        .iter()
        .filter(|r| {
            !r.bpm_ok
                && r.bpm > 0.0
                && ((r.bpm * 2.0 - r.bpm_gt).abs() < 0.1 || (r.bpm / 2.0 - r.bpm_gt).abs() < 0.1)
        })
        .count();
    println!(
        "BPM within 0.05:         {}   (octave errors: {octave})",
        pct(bpm_ok, n)
    );
    let ok: Vec<&Res> = rs.iter().filter(|r| r.bpm_ok).collect();
    let ph15 = ok.iter().filter(|r| r.phase_ms.abs() < 15.0).count();
    let ph5 = ok.iter().filter(|r| r.phase_ms.abs() < 5.0).count();
    println!(
        "beat phase within 15 ms: {}   (5 ms: {})  [of all: {}]",
        pct(ph15, ok.len()),
        pct(ph5, ok.len()),
        pct(ph15, n)
    );
    let mut offs: Vec<f64> = ok
        .iter()
        .filter(|r| r.phase_ms.abs() < 40.0)
        .map(|r| r.phase_ms)
        .collect();
    offs.sort_by(|a, b| a.total_cmp(b));
    if !offs.is_empty() {
        println!(
            "phase offset ms (ours-GT, |x|<40): median {:+.1}, p10 {:+.1}, p90 {:+.1}",
            offs[offs.len() / 2],
            offs[offs.len() / 10],
            offs[offs.len() * 9 / 10]
        );
    }
    let beat_ok: Vec<&&Res> = ok.iter().filter(|r| r.phase_ms.abs() < 40.0).collect();
    let bar_ok = beat_ok.iter().filter(|r| r.bar_off == 0).count();
    let mut bar_hist = [0usize; 4];
    for r in &beat_ok {
        if r.bar_off >= 0 {
            bar_hist[r.bar_off as usize] += 1;
        }
    }
    println!(
        "bar phase == DJ.Studio:  {}   offsets(beats) {:?}",
        pct(bar_ok, beat_ok.len()),
        bar_hist
    );
    let refs: Vec<bool> = beat_ok.iter().filter_map(|r| r.bar_ref_ok).collect();
    println!(
        "bar phase vs reference:  {}   (MIK cues where available, else DJ.Studio)",
        pct(refs.iter().filter(|x| **x).count(), refs.len())
    );
    let mb: Vec<f64> = beat_ok.iter().filter_map(|r| r.mik_bar).collect();
    if !mb.is_empty() {
        let good = mb.iter().filter(|x| **x >= 0.5).count();
        println!(
            "bar phase vs MIK cues:   {}   (mean cue-on-bar-line {:.2})",
            pct(good, mb.len()),
            mb.iter().sum::<f64>() / mb.len() as f64
        );
    }
    let kn = all.iter().filter(|r| r.key_rel != "none").count();
    let count = |s: &str| all.iter().filter(|r| r.key_rel == s).count();
    println!(
        "key: exact {}  relative {}  fifth {}  parallel {}  other {}   exact+rel {}",
        pct(count("exact"), kn),
        count("relative"),
        count("fifth"),
        count("parallel"),
        count("other"),
        pct(count("exact") + count("relative"), kn)
    );
    let rec: Vec<f64> = all.iter().filter_map(|r| r.mik_recall).collect();
    if !rec.is_empty() {
        println!(
            "MIK cue recall (±1 bar):     {:.2} over {} tracks",
            rec.iter().sum::<f64>() / rec.len() as f64,
            rec.len()
        );
    }
    let raw: Vec<(f64, f64, f64)> = all.iter().filter_map(|r| r.raw).collect();
    if !raw.is_empty() {
        let m = |f: fn(&(f64, f64, f64)) -> f64| raw.iter().map(f).sum::<f64>() / raw.len() as f64;
        println!(
            "raw boundaries (before labels/merge): MIK segment recall {:.2} / precision {:.2}, MIK cue recall {:.2} over {} tracks",
            m(|x| x.0),
            m(|x| x.1),
            m(|x| x.2),
            raw.len()
        );
    }
    let sr: Vec<f64> = all.iter().filter_map(|r| r.seg_recall).collect();
    if !sr.is_empty() {
        let sp: Vec<f64> = all.iter().filter_map(|r| r.seg_precision).collect();
        println!(
            "MIK segment boundaries: recall {:.2}, precision {:.2} over {} tracks",
            sr.iter().sum::<f64>() / sr.len() as f64,
            sp.iter().sum::<f64>() / sp.len().max(1) as f64,
            sr.len()
        );
    }
    let en: Vec<(f64, f64)> = all
        .iter()
        .filter_map(|r| r.mik_energy.map(|m| (r.energy as f64, m as f64)))
        .collect();
    if !en.is_empty() {
        let mae = en.iter().map(|(a, b)| (a - b).abs()).sum::<f64>() / en.len() as f64;
        println!("energy vs MIK: MAE {:.2} over {} tracks", mae, en.len());
    }
    let mut tg: Vec<f64> = all
        .iter()
        .filter_map(|r| r.dj_target)
        .filter(|x| x.is_finite())
        .collect();
    tg.sort_by(|a, b| a.total_cmp(b));
    if !tg.is_empty() {
        println!(
            "implied DJ.Studio loudness target: median {:.2} LUFS",
            tg[tg.len() / 2]
        );
    }
    let mi: Vec<f64> = all.iter().filter_map(|r| r.mix_in_bars).collect();
    let mo: Vec<f64> = all.iter().filter_map(|r| r.mix_out_bars).collect();
    if !mi.is_empty() {
        let w = |v: &[f64], b: f64| v.iter().filter(|x| x.abs() <= b).count();
        println!(
            "vs DJ.Studio system cues: mix-in exact {} / within 8 bars {}, mix-out exact {} / within 8 bars {}",
            pct(w(&mi, 0.5), mi.len()),
            pct(w(&mi, 8.0), mi.len()),
            pct(w(&mo, 0.5), mo.len()),
            pct(w(&mo, 8.0), mo.len())
        );
    }
    let mut ts: Vec<f64> = all.iter().map(|r| r.secs).collect();
    ts.sort_by(|a, b| a.total_cmp(b));
    if !ts.is_empty() {
        println!(
            "analysis time: median {:.2} s, max {:.2} s",
            ts[ts.len() / 2],
            ts[ts.len() - 1]
        );
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let mut db = home().join("Music/DJ.Studio/Database");
    let mut stems_root: Option<PathBuf> = None;
    let mut only_stems = false;
    let mut filter: Option<String> = None;
    let mut limit = usize::MAX;
    let mut csv: Option<PathBuf> = None;
    let mut verbose = false;
    let mut dump_onoff = false;
    let mut dump_phase = false;
    let mut dump_chroma = false;
    let mut only_mik = false;
    let mut jobs = 4usize;
    let mut make_stems = false;
    let mut act_cache: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--db" => db = PathBuf::from(args.next().unwrap()),
            "--stems-root" => stems_root = args.next().map(PathBuf::from),
            "--only-stems" => only_stems = true,
            "--filter" => filter = args.next(),
            "--limit" => limit = args.next().unwrap().parse()?,
            "--csv" => csv = args.next().map(PathBuf::from),
            "-v" => verbose = true,
            "--dump-onoff" => dump_onoff = true,
            "--dump-phase" => dump_phase = true,
            "--dump-chroma" => dump_chroma = true,
            "--only-mik" => only_mik = true,
            "--jobs" => jobs = args.next().unwrap().parse()?,
            "--make-stems" => make_stems = true,
            "--act-cache" => act_cache = args.next().map(PathBuf::from),
            _ => anyhow::bail!("unknown argument {a}"),
        }
    }
    let index = index_audio(&[
        home().join("Music"),
        home().join("Downloads"),
        home().join("Desktop/New_Selected_mix"),
    ]);
    let mut truth = load_truth(&db, &index);
    if let Some(f) = &filter {
        truth.retain(|t| t.name.contains(f.as_str()));
    }
    if only_mik {
        truth.retain(|t| !t.mik_cues.is_empty());
    }
    let flac_path = |t: &Truth| -> Option<PathBuf> {
        let root = stems_root.as_ref()?;
        Some(root.join(format!(
            "{}.stems.flac",
            t.path.file_stem()?.to_string_lossy()
        )))
    };
    let stem_dir = |t: &Truth| -> Option<PathBuf> {
        let root = stems_root.as_ref()?;
        let stem = t.path.file_stem()?.to_string_lossy().to_string();
        let f = root.join(format!("{stem}.stems.flac"));
        if f.exists() {
            return Some(f);
        }
        let d = root.join(stem);
        (d.join("drums.mp3").exists() || d.join("drums.wav").exists()).then_some(d)
    };
    truth.truncate(limit);
    if make_stems {
        // Rust HTDemucs (Quality::Fast), one track at a time.
        let todo: Vec<&Truth> = truth.iter().filter(|t| stem_dir(t).is_none()).collect();
        if !todo.is_empty() {
            let sep = clubcraft_stems::Separator::new(clubcraft_stems::Quality::Fast)?;
            for (i, t) in todo.iter().enumerate() {
                let Some(dst) = flac_path(t) else { continue };
                let audio = match clubcraft_audio::decode_file(&t.path, 44_100) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("skip {}: {e:#}", t.name);
                        continue;
                    }
                };
                let t0 = Instant::now();
                let st = sep.separate(&audio, &mut |_| true)?;
                std::fs::create_dir_all(dst.parent().unwrap())?;
                clubcraft_stems::save_stems_with_model(&dst, &st, sep.model_name())?;
                eprintln!(
                    "stems {}/{}: {} ({:.1?}, {})",
                    i + 1,
                    todo.len(),
                    t.name,
                    t0.elapsed(),
                    sep.backend_name()
                );
            }
        }
    }
    if only_stems {
        truth.retain(|t| stem_dir(t).is_some());
    }
    println!("{} DJ.Studio-analysed tracks found locally", truth.len());
    let model: std::sync::OnceLock<Option<clubcraft_analysis::allinone::Model>> =
        std::sync::OnceLock::new();
    let model_lock = std::sync::Mutex::new(());
    let get_act = |t: &Truth,
                   st: &clubcraft_analysis::OwnedStems|
     -> Option<clubcraft_analysis::allinone::Activations> {
        let cache = act_cache.as_ref().map(|d| {
            d.join(format!(
                "{}.act.npy",
                t.path.file_stem().unwrap().to_string_lossy()
            ))
        });
        if let Some(c) = &cache {
            if let Ok(a) = clubcraft_stems::npy::read_npy(c) {
                return Some(clubcraft_analysis::allinone::Activations::from_flat(
                    &a.data,
                ));
            }
        }
        let m = model
            .get_or_init(|| {
                clubcraft_analysis::allinone::Model::load_default()
                    .map_err(|e| eprintln!("model: {e:#}"))
                    .ok()
            })
            .as_ref()?;
        // One model run at a time (memory).
        let _g = model_lock.lock().unwrap();
        let t0 = Instant::now();
        let a = m
            .activations(&st.as_ref())
            .map_err(|e| eprintln!("model {}: {e:#}", t.name))
            .ok()?;
        eprintln!("model {}: {:.1?}", t.name, t0.elapsed());
        if let Some(c) = &cache {
            let mut flat = Vec::with_capacity(a.len() * 13);
            for i in 0..a.len() {
                flat.extend_from_slice(&[a.beat[i], a.downbeat[i], a.section[i]]);
                flat.extend_from_slice(&a.labels[i]);
            }
            let _ = std::fs::create_dir_all(c.parent().unwrap());
            let _ = clubcraft_stems::npy::write_npy(c, &[a.len(), 13], &flat);
        }
        Some(a)
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.max(1))
        .build()?;
    let results: Vec<(Res, Option<Res>, Option<Res>)> = pool.install(|| {
        truth
            .par_iter()
            .with_max_len(1)
            .filter_map(|t| {
                let audio = match clubcraft_audio::decode_file(&t.path, 44_100) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("decode {}: {e}", t.path.display());
                        return None;
                    }
                };
                let opts = AnalyzeOptions::default();
                let t0 = Instant::now();
                let a = analyze(&audio, None, &opts);
                let r = evaluate(t, &a, false, t0.elapsed().as_secs_f64(), verbose);
                if dump_chroma {
                    if let (Some((tun, tb, bb)), Some(k)) = (
                        clubcraft_analysis::key::debug_chroma(&audio, None),
                        t.mik_key.or(t.key),
                    ) {
                        let f = |c: &[f32; 12]| {
                            c.iter()
                                .map(|v| format!("{v:.4}"))
                                .collect::<Vec<_>>()
                                .join(",")
                        };
                        println!(
                            "CHROMA {} {} {} {tun:.1} {} {} |{}",
                            k.tonic,
                            k.minor as u8,
                            if t.mik_key.is_some() { "mik" } else { "dj" },
                            f(&tb),
                            f(&bb),
                            t.name
                        );
                    }
                }
                if dump_phase {
                    if let Some((first, beat, f)) =
                        clubcraft_analysis::beatgrid::debug_phase_features(&audio)
                    {
                        let spb = 60.0 / t.bpm;
                        // Reference downbeat: MIK cues (majority bar line) if present, else DJ.Studio beat 0.
                        let mut votes = [0usize; 4];
                        for b in &t.mik_cues {
                            votes[(*b).rem_euclid(4) as usize] += 1;
                        }
                        let mik_shift = (0..4).max_by_key(|i| votes[*i]).unwrap_or(0) as f64;
                        let src = if t.mik_cues.is_empty() { "dj " } else { "mik" };
                        let gt_db = t.beat0
                            + if t.mik_cues.is_empty() {
                                0.0
                            } else {
                                mik_shift * spb
                            };
                        let k = (gt_db - first) / beat;
                        if (k - k.round()).abs() < 0.2 && (t.bpm - 60.0 / beat).abs() < 0.05 {
                            let p = (k.round() as i64).rem_euclid(4);
                            let lv: Vec<String> = (0..6)
                                .map(|k| {
                                    let a = ((first + k as f64 * beat) * 44100.0) as usize;
                                    let b = (a + (beat * 44100.0) as usize).min(audio.left.len());
                                    let ms =
                                        audio.left[a.min(b)..b].iter().map(|v| v * v).sum::<f32>()
                                            / (b - a).max(1) as f32;
                                    format!("{:.0}", 10.0 * ms.max(1e-12).log10())
                                })
                                .collect();
                            eprintln!(
                                "FIRST {src} p={p} first={first:.3} beat0={:.3} lv={} |{}",
                                t.beat0,
                                lv.join(","),
                                t.name
                            );
                            let mut line =
                                format!("PHASE {src} {p} {:.3}", (first - t.beat0) / spb);
                            for row in &f {
                                line += &format!(
                                    " {:.3},{:.3},{:.3},{:.3}",
                                    row[0], row[1], row[2], row[3]
                                );
                            }
                            println!("{line} |{}", t.name);
                        }
                    }
                }
                if dump_onoff {
                    if let Some(g) = a.grid {
                        let p = clubcraft_analysis::beatgrid::debug_pulse_profile(&audio, &g);
                        let half = (r.phase_ms.abs() - 30000.0 / r.bpm_gt).abs() < 25.0;
                        let ok = r.phase_ms.abs() < 25.0;
                        let mut line = format!(
                            "ONOFF {} ",
                            if ok {
                                "on "
                            } else if half {
                                "off"
                            } else {
                                "?? "
                            }
                        );
                        for v in &p {
                            line += &format!(" {:.2}/{:.2}", v[0], v[1]);
                        }
                        println!("{line}  {}", t.name);
                    }
                }
                let stems = stem_dir(t).and_then(|d| {
                    if d.extension().is_some_and(|e| e == "flac") {
                        clubcraft_stems::load_stems(&d)
                            .ok()
                            .map(|s| clubcraft_analysis::OwnedStems { stems: s.stems })
                    } else {
                        common::load_stems(&d, &audio).ok()
                    }
                });
                let (mut rs, mut rm) = (None, None);
                if let Some(st) = &stems {
                    let rules = AnalyzeOptions {
                        structure: clubcraft_analysis::StructureSource::Rules,
                        ..AnalyzeOptions::default()
                    };
                    let t0 = Instant::now();
                    let a = analyze(&audio, Some(&st.as_ref()), &rules);
                    let mut r1 = evaluate(t, &a, true, t0.elapsed().as_secs_f64(), verbose);
                    let bars = a.grid.map(|g| {
                        (
                            g,
                            clubcraft_analysis::structure::bar_features_for(
                                &audio,
                                Some(st.as_ref()),
                                &g,
                            ),
                        )
                    });
                    let times = |b: &clubcraft_analysis::structure::BarFeatures,
                                 idx: &[usize]|
                     -> Vec<f64> {
                        idx.iter()
                            .filter(|i| **i < b.len())
                            .map(|i| b.start[*i])
                            .collect()
                    };
                    if let Some((_, b)) = &bars {
                        r1.raw = raw_scores(
                            t,
                            &times(b, &clubcraft_analysis::structure::rule_boundaries(b)),
                        );
                    }
                    rs = Some(r1);
                    if let Some(act) = get_act(t, st) {
                        let t0 = Instant::now();
                        let a = clubcraft_analysis::analyze_with_activations(
                            &audio,
                            Some(&st.as_ref()),
                            &AnalyzeOptions::default(),
                            Some(&act),
                        );
                        let mut r2 = evaluate(t, &a, true, t0.elapsed().as_secs_f64(), verbose);
                        if let Some((g, b)) = &bars {
                            r2.raw = raw_scores(
                                t,
                                &times(
                                    b,
                                    &clubcraft_analysis::structure::model_boundaries(b, g, &act),
                                ),
                            );
                        }
                        rm = Some(r2);
                    }
                }
                Some((r, rs, rm))
            })
            .collect()
    });
    let mut plain: Vec<Res> = results.iter().map(|r| r.0.clone()).collect();
    plain.sort_by(|a, b| a.name.cmp(&b.name));
    println!(
        "\n{:<44} {:>8} {:>8} {:>7} {:>3} {:>4} {:>4} {:>4} {:>8} {:>4} {:>4} {:>5} {:>5}",
        "track", "bpm", "gt", "ph ms", "bar", "key", "gt", "", "", "mikB", "rec", "seg", "en/m"
    );
    let print = |r: &Res| {
        println!(
            "{:<44} {:>8.3} {:>8.3} {:>7.1} {:>3} {:>4} {:>4} {:>8} {:>4} {:>4} {:>5} {:>5}{}",
            r.name.chars().take(44).collect::<String>(),
            r.bpm,
            r.bpm_gt,
            r.phase_ms,
            r.bar_off,
            r.key,
            r.key_gt,
            r.key_rel,
            r.mik_bar.map(|x| format!("{x:.2}")).unwrap_or_default(),
            r.mik_recall.map(|x| format!("{x:.2}")).unwrap_or_default(),
            r.seg_recall.map(|x| format!("{x:.2}")).unwrap_or_default(),
            format!(
                "{}/{}",
                r.energy,
                r.mik_energy.map(|m| m.to_string()).unwrap_or("-".into())
            ),
            if r.detail.is_empty() {
                String::new()
            } else {
                format!("\n    {}", r.detail)
            }
        );
    };
    for r in &plain {
        print(r);
    }
    let stemmed: Vec<(Res, Res)> = results
        .iter()
        .filter_map(|r| r.1.clone().map(|s| (r.0.clone(), s)))
        .collect();
    let modeled: Vec<(Res, Res, Res)> = results
        .iter()
        .filter_map(|r| Some((r.0.clone(), r.1.clone()?, r.2.clone()?)))
        .collect();
    if !stemmed.is_empty() {
        println!("\n--- with stems, rule-based structure ---");
        for (_, s) in &stemmed {
            print(s);
        }
    }
    if !modeled.is_empty() {
        println!("\n--- with stems, All-In-One structure ---");
        for (_, _, m) in &modeled {
            print(m);
        }
    }
    summarize("without stems", &plain);
    if !stemmed.is_empty() {
        let a: Vec<Res> = stemmed.iter().map(|x| x.0.clone()).collect();
        let b: Vec<Res> = stemmed.iter().map(|x| x.1.clone()).collect();
        summarize("stem subset, mix only", &a);
        summarize("stem subset, with stems (rules)", &b);
    }
    if !modeled.is_empty() {
        let b: Vec<Res> = modeled.iter().map(|x| x.1.clone()).collect();
        let c: Vec<Res> = modeled.iter().map(|x| x.2.clone()).collect();
        summarize("model subset, with stems (rules)", &b);
        summarize("model subset, with stems (All-In-One)", &c);
    }
    if let Some(p) = csv {
        let mut s = String::from("name,stems,bpm,bpm_gt,phase_ms,bar_off,key,key_gt,key_rel,mik_bar,mik_recall,seg_recall,energy,mik_energy,lufs,secs\n");
        for r in plain.iter().chain(stemmed.iter().map(|x| &x.1)) {
            s += &format!(
                "\"{}\",{},{:.4},{:.4},{:.2},{},{},{},{},{},{},{},{},{},{:.2},{:.3}\n",
                r.name.replace('"', "'"),
                r.stems,
                r.bpm,
                r.bpm_gt,
                r.phase_ms,
                r.bar_off,
                r.key,
                r.key_gt,
                r.key_rel,
                r.mik_bar.map(|x| x.to_string()).unwrap_or_default(),
                r.mik_recall.map(|x| x.to_string()).unwrap_or_default(),
                r.seg_recall.map(|x| x.to_string()).unwrap_or_default(),
                r.energy,
                r.mik_energy.map(|x| x.to_string()).unwrap_or_default(),
                r.lufs,
                r.secs
            );
        }
        std::fs::write(p, s)?;
    }
    Ok(())
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn djstudio_key_numbers() {
        // a record with key number 14 is tagged "10A" (B minor); 22 matches ClubCraft's G minor
        assert_eq!(camelot_int_to_key(14).unwrap().camelot(), "10A");
        assert_eq!(camelot_int_to_key(22), Some(Key::new(7, true)));
        assert_eq!(camelot_int_to_key(12), Some(Key::new(9, true)));
        assert_eq!(camelot_int_to_key(0), Some(Key::new(0, false)));
        assert_eq!(camelot_int_to_key(9), Some(Key::new(9, false)));
        assert_eq!(camelot_int_to_key(-1), None);
        assert_eq!(camelot_int_to_key(24), None);
    }
}
