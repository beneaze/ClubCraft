//! Why does (or doesn't) the finder pick a given mashup? Scores the vocals of one track from a
//! bar over the instrumental of another from a bar, part by part, and shows where the pair ranks.
//!
//! `cargo run --release -p clubcraft-analysis --example mashup_pair -- <library.db> <Mashup dir> <vocal id> <vocal bar> <inst id> <inst bar> [bars]`
//!
//! Bars are 1-based, as the app shows them. Reads the cached features, vocal maps and labels.
//! `REVERSE=<scratch dir>`: rank the pair as the finder does with the instrumental track first
//! (every other track's vocals over it); missing vocal maps are computed and kept in the dir.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine

use clubcraft_analysis::labels::{BarLabels, Label};
use clubcraft_analysis::mashup::{self, MashupFeatures, MatchOptions};
use clubcraft_analysis::vocals::VocalMap;
use clubcraft_core::Library;

fn load(lib: &Library, dir: &std::path::Path, id: i64) -> MashupFeatures {
    let t = lib.track(id).unwrap().unwrap();
    let a = lib.analysis(id).unwrap().unwrap();
    let mut f = MashupFeatures::from_bytes(&std::fs::read(dir.join(format!("{}.ccmf", t.hash))).unwrap()).unwrap();
    f.set_sections(&a.sections);
    if let Some(m) = std::fs::read(dir.join(format!("{}.ccvm", t.hash))).ok().and_then(|b| VocalMap::from_bytes(&b)) {
        f.set_vocal_map(m);
    }
    if let Some(l) = std::fs::read(dir.join(format!("{}.ccfl", t.hash))).ok().and_then(|b| BarLabels::from_bytes(&b)) {
        f.set_labels(l);
    }
    f
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let lib = Library::open(a[1].as_ref()).unwrap();
    let dir = std::path::PathBuf::from(&a[2]);
    let (vid, vbar, iid, ibar): (i64, i32, i64, i32) = (a[3].parse().unwrap(), a[4].parse::<i32>().unwrap() - 1, a[5].parse().unwrap(), a[6].parse::<i32>().unwrap() - 1);
    let bars: usize = a.get(7).and_then(|s| s.parse().ok()).unwrap_or(16);
    let (v, i) = (load(&lib, &dir, vid), load(&lib, &dir, iid));
    let name = |id: i64| lib.track(id).unwrap().unwrap().display_name();
    println!("vocals {} (map {}, labels {}), instrumental {}", name(vid), v.vocal_map.is_some(), v.labels.is_some(), name(iid));
    let o = MatchOptions { bars, ..Default::default() };
    println!("\nvocal units the finder considers ({bars} bars):");
    for u in mashup::vocal_units(&v, &o, 12) {
        println!("  bar {:>3} x{:<2} {:<6} q {:.2}", u.bar + 1, u.bars, u.role.label(), u.quality);
    }
    for n in [8usize, 16, 32] {
        let q = mashup::unit_quality(&v, vbar, n);
        let p = v.vocal_map.as_ref().map(|m| m.phrase(vbar, n)).unwrap_or_default();
        let chorus = v.labels.as_ref().map(|l| l.mean(vbar, n, Label::Chorus)).unwrap_or(0.0);
        let verse = v.labels.as_ref().map(|l| l.mean(vbar, n, Label::Verse)).unwrap_or(0.0);
        println!("  bar {} x{n}: quality {q:.2} | P(chorus) {chorus:.2} P(verse) {verse:.2} | cover {:.2} start {:.2} end {:.2} lyrical {:.2} hook {:.2}", vbar + 1, p.cover, p.start, p.end, p.lyrical, p.hook);
    }
    if let Some(m) = &v.vocal_map {
        // the cached map around the vocal bar: blank = break between lines, then level
        for bar in vbar - 2..vbar + 2 {
            let s = m.step_of_bar(bar);
            let line: String = (s..s + m.steps_per_bar()).map(|k| if m.is_break(k) { ' ' } else if m.is_quiet(k) { '_' } else { match m.level_at(k) { x if x > -6.0 => '█', x if x > -12.0 => '▓', x if x > -20.0 => '▒', _ => '·' } }).collect();
            let sung: Vec<String> = (s..s + m.steps_per_bar()).map(|k| format!("{:.1}", m.sung_at(k))).collect();
            println!("  bar {:>3} |{line}| sung {}", bar + 1, sung.join(" "));
        }
        println!("  first step {} (bar {} starts at step {})", m.first_step, vbar + 1, m.step_of_bar(vbar));
    }
    println!("\ninstrumental sections:");
    for s in &i.sections {
        println!("  bar {:>3} x{:<3} {:<6} energy {:.2}", s.start_bar + 1, s.bars, s.kind.label(), s.energy);
    }
    for n in [16usize, 32] {
        let o = MatchOptions { bars: n, key_range: 2, ..Default::default() };
        for k in -2..=2 {
            if let Some(m) = mashup::score(&v, vbar, &i, ibar, k, &o) {
                println!("  x{n} key {k:+}: score {:.2} fit {:.2} (harmony {:.2} min {:.2} mask {:.2} resid {:.2}) placement {:.2} phrase {:.2} cost {:.2}", m.score, m.fit, m.harmony, m.harmony_min, m.masking, m.residual_vocals, m.placement, m.phrase, m.cost);
            }
        }
    }
    if let Ok(scratch) = std::env::var("REVERSE") {
        reverse(&lib, &dir, std::path::Path::new(&scratch), vid, iid, &i, bars);
        return;
    }
    // where the pair ranks in a search over the library
    let analyses = lib.all_analyses().unwrap();
    let mut feats: Vec<(i64, MashupFeatures)> = Vec::new();
    for t in lib.tracks().unwrap() {
        let Some(an) = analyses.get(&t.id) else { continue };
        let Some(mut f) = std::fs::read(dir.join(format!("{}.ccmf", t.hash))).ok().and_then(|b| MashupFeatures::from_bytes(&b)) else { continue };
        f.set_sections(&an.sections);
        feats.push((t.id, f));
    }
    let cands: Vec<(i64, &MashupFeatures)> = feats.iter().map(|(id, f)| (*id, f)).collect();
    let o = MatchOptions { bars, max_results: 400, per_track: 4, ..Default::default() };
    let found = mashup::find((vid, &v), &mashup::vocal_units(&v, &o, 6), &cands, &o);
    let pos = found.iter().position(|m| m.inst_track == iid);
    println!("\nsearch: {} matches; best with the instrumental: {:?}", found.len(), pos.map(|p| (p + 1, found[p].vocal_bar + 1, found[p].inst_bar + 1, found[p].score)));
    for m in found.iter().take(5) {
        println!("  {:.2} vocal bar {:>3} x{} over {} bar {}", m.score, m.vocal_bar + 1, m.bars, name(m.inst_track).chars().take(40).collect::<String>(), m.inst_bar + 1);
    }
}

/// The finder with the instrumental first: every other track's vocals over `inst`.
fn reverse(lib: &Library, dir: &std::path::Path, scratch: &std::path::Path, vid: i64, iid: i64, inst: &MashupFeatures, bars: usize) {
    let analyses = lib.all_analyses().unwrap();
    let o = MatchOptions { bars, max_results: 400, order: mashup::MashupOrder::InstrumentalFirst, ..Default::default() };
    let mut found = Vec::new();
    for t in lib.tracks().unwrap() {
        let (Some(an), true) = (analyses.get(&t.id), t.id != iid) else { continue };
        let Some(mut f) = std::fs::read(dir.join(format!("{}.ccmf", t.hash))).ok().and_then(|b| MashupFeatures::from_bytes(&b)) else { continue };
        if mashup::tempo_ratio(f.bpm, inst.bpm, o.tempo_tolerance).is_none() {
            continue;
        }
        f.set_sections(&an.sections);
        let cached = [dir, scratch].iter().find_map(|d| std::fs::read(d.join(format!("{}.ccvm", t.hash))).ok().and_then(|b| VocalMap::from_bytes(&b)));
        let map = cached.unwrap_or_else(|| {
            let st = clubcraft_stems::load_stem_track(&clubcraft_core::library::stems_path(&t.hash)).unwrap();
            let owned = clubcraft_analysis::OwnedStems::from_stem_track_partial(&st);
            let m = clubcraft_analysis::vocals::compute(&owned.as_ref(), an.grid.unwrap(), an.duration);
            std::fs::write(scratch.join(format!("{}.ccvm", t.hash)), m.to_bytes()).unwrap();
            m
        });
        f.set_vocal_map(map);
        if let Some(l) = [dir, scratch].iter().find_map(|d| std::fs::read(d.join(format!("{}.ccfl", t.hash))).ok().and_then(|b| BarLabels::from_bytes(&b))) {
            f.set_labels(l);
        }
        let units = mashup::vocal_units(&f, &o, 4);
        found.extend(mashup::find((t.id, &f), &units, &[(iid, inst)], &o));
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let name = |id: i64| lib.track(id).unwrap().unwrap().display_name().chars().take(40).collect::<String>();
    let pos = found.iter().position(|m| m.vocal_track == vid);
    println!("\ninstrumental first: {} matches; best with these vocals: {:?}", found.len(), pos.map(|p| (p + 1, found[p].vocal_bar + 1, found[p].inst_bar + 1, found[p].score)));
    for m in found.iter().take(8) {
        println!("  {:.2} vocals of {:<40} bar {:>3} x{} ({}) over bar {}", m.score, name(m.vocal_track), m.vocal_bar + 1, m.bars, m.vocal_role.label(), m.inst_bar + 1);
    }
}
