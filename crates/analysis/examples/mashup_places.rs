//! Where does the mashup finder put the vocals? Lists the best placements for some vocal tracks
//! with the section kind of the instrumental at that bar.
//!
//! `cargo run --release -p clubcraft-analysis --example mashup_places -- <library.db> <Mashup dir> [n vocal tracks]`
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
    let names: HashMap<i64, String> = feats.iter().map(|(i, n, _)| (*i, n.clone())).collect();
    let cands: Vec<(i64, &MashupFeatures)> = feats.iter().map(|(i, _, f)| (*i, f)).collect();
    let o = MatchOptions::default();
    let mut hist: HashMap<String, usize> = HashMap::new();
    for (vid, vname, v) in feats.iter().step_by((feats.len() / nv.max(1)).max(1)).take(nv) {
        let regions = mashup::vocal_regions(v, &o, 3);
        let va = &analyses[vid];
        println!("\n== vocals of {vname}  ({} bars, phrases {:?})", v.bars.len(), regions.iter().map(|r| format!("{} {}", r + 1, kind_at(va, *r))).collect::<Vec<_>>());
        for m in mashup::find((*vid, v), &regions, &cands, &o).iter().take(6) {
            let ia = &analyses[&m.inst_track];
            let f = &feats.iter().find(|x| x.0 == m.inst_track).unwrap().2;
            let k = kind_at(ia, m.inst_bar);
            *hist.entry(k.trim_end_matches('*').to_string()).or_default() += 1;
            println!(
                "  {:.2} h {:.2} pl {:.2} | vocal bar {:>3} ({:<6}) over {:<36} bar {:>3}/{:<3} {:<7} key {:+}",
                m.score,
                m.harmony,
                m.placement,
                m.vocal_bar + 1,
                kind_at(va, m.vocal_bar),
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
