//! Where does the mashup finder put the vocals? Lists the best placements for some vocal tracks
//! with the section kind of the instrumental at that bar.
//!
//! `cargo run --release -p clubcraft-analysis --example mashup_places -- <library.db> <Mashup dir> [n vocal tracks] [label cache dir]`
//!
//! The vocal tracks get their vocal map and the structure model's labels (~30 s each; cached in
//! the label cache dir when given), as the finder computes them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine

use clubcraft_analysis::mashup::{self, MashupFeatures, MatchOptions};
use clubcraft_core::{Analysis, Library};
use std::collections::HashMap;

fn kind_at(a: &Analysis, bar: i32) -> String {
    a.sections.iter().find(|s| bar >= s.start_bar && bar < s.start_bar + s.bars as i32).map(|s| format!("{}{}", s.kind.label(), if s.start_bar == bar { "*" } else { "" })).unwrap_or_else(|| "?".into())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let lib = Library::open(args[1].as_ref()).unwrap();
    let dir = std::path::PathBuf::from(&args[2]);
    let nv: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(8);
    let analyses = lib.all_analyses().unwrap();
    let mut feats: Vec<(i64, String, MashupFeatures)> = Vec::new();
    for t in lib.tracks().unwrap() {
        let Some(a) = analyses.get(&t.id) else { continue };
        let Ok(b) = std::fs::read(dir.join(format!("{}.ccmf", t.hash))) else { continue };
        let Some(mut f) = MashupFeatures::from_bytes(&b) else { continue };
        f.set_sections(&a.sections);
        feats.push((t.id, t.display_name().chars().take(36).collect(), f));
    }
    if let Ok(pat) = std::env::var("DUMP") {
        // per bar: section, vocal level (dB under the track's p95), dominant vocal note, phrase quality
        let notes = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        for (id, name, f) in feats.iter().filter(|x| x.1.to_lowercase().contains(&pat.to_lowercase())) {
            let a = &analyses[id];
            let pk = f.bars.iter().map(|b| b.vocal_db).fold(-90.0f32, f32::max);
            println!("\n== {name}  chosen {:?}", mashup::vocal_regions(f, &MatchOptions::default(), 4).iter().map(|b| b + 1).collect::<Vec<_>>());
            for (j, b) in f.bars.iter().enumerate() {
                let bar = f.first_bar + j as i32;
                let top = (0..12).max_by(|x, y| b.vocal[*x].total_cmp(&b.vocal[*y])).unwrap();
                let lvl = (b.vocal_db - pk).max(-60.0);
                println!("{:>4} {:<8} {:>6.1} {} {:<3} q {:.2}", bar + 1, kind_at(a, bar), lvl, "#".repeat(((lvl + 40.0).max(0.0) / 2.0) as usize), notes[top], mashup::phrase_quality(f, bar, 16));
            }
        }
        return;
    }
    let names: HashMap<i64, String> = feats.iter().map(|(i, n, _)| (*i, n.clone())).collect();
    let cands: Vec<(i64, &MashupFeatures)> = feats.iter().map(|(i, _, f)| (*i, f)).collect();
    let o = MatchOptions::default();
    let mut hist: HashMap<String, usize> = HashMap::new();
    let picks: Vec<(i64, String, MashupFeatures)> = feats.iter().step_by((feats.len() / nv.max(1)).max(1)).take(nv).cloned().collect();
    for (vid, vname, mut vf) in picks {
        // the vocal map, as the finder computes it for the vocal track
        let t = lib.track(vid).unwrap().unwrap();
        let st = clubcraft_stems::load_stem_track(&clubcraft_core::library::stems_path(&t.hash)).unwrap();
        let owned = clubcraft_analysis::OwnedStems::from_stem_track_partial(&st);
        let (grid, dur) = (analyses[&vid].grid.unwrap(), analyses[&vid].duration);
        vf.set_vocal_map(clubcraft_analysis::vocals::compute(&owned.as_ref(), grid, dur));
        let cache = args.get(4).map(|d| std::path::Path::new(d).join(format!("{}.ccfl", t.hash)));
        let cached = cache.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| clubcraft_analysis::labels::BarLabels::from_bytes(&b));
        let labels = cached.or_else(|| {
            let l = clubcraft_analysis::allinone::shared_activations(&owned.as_ref()).map(|a| clubcraft_analysis::labels::BarLabels::from_activations(&a, grid, dur));
            if let (Some(l), Some(p)) = (&l, &cache) {
                std::fs::write(p, l.to_bytes()).unwrap();
            }
            l
        });
        if let Some(l) = labels {
            vf.set_labels(l);
        }
        let (vid, vname, v) = (&vid, &vname, &vf);
        let model = analyses[vid].stems_model.clone().unwrap_or_default();
        let units = mashup::vocal_units(v, &o, 6);
        if std::env::var("PHRASES").is_ok() {
            let q: Vec<String> = (0..v.bars.len() as i32 - 16).step_by(4).map(|b| format!("{}:{:.2}", b + 1, mashup::phrase_quality(v, b, 16))).collect();
            println!("{}", q.join(" "));
        }
        let va = &analyses[vid];
        let g = va.grid.unwrap();
        println!("\n== vocals of {vname} [{model}]  ({} bars, phrases {:?})", v.bars.len(), units.iter().map(|u| { let t = g.time_of_bar(u.bar as f64); format!("bar {} {}:{:02} {}x{} {} q {:.2}", u.bar + 1, (t / 60.0) as i32, (t % 60.0) as i32, u.role.label(), u.bars, kind_at(va, u.bar), u.quality) }).collect::<Vec<_>>());
        for m in mashup::find((*vid, v), &units, &cands, &o).iter().take(6) {
            let ia = &analyses[&m.inst_track];
            let f = &feats.iter().find(|x| x.0 == m.inst_track).unwrap().2;
            let k = kind_at(ia, m.inst_bar);
            *hist.entry(k.trim_end_matches('*').to_string()).or_default() += 1;
            println!(
                "  {:.2} fit {:.2} pl {:.2} ph {:.2} | vocal bar {:>3} ({:<6} {}x{}) over {:<36} bar {:>3}/{:<3} {:<7} key {:+}",
                m.score,
                m.fit,
                m.placement,
                m.phrase,
                m.vocal_bar + 1,
                kind_at(va, m.vocal_bar),
                m.vocal_role.label(),
                m.bars,
                names[&m.inst_track],
                m.inst_bar + 1,
                f.last_bar(),
                k,
                m.key_shift
            );
        }
    }
    println!("\nsection kinds of the top placements: {hist:?}");
}
