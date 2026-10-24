//! Per bar of a track: how much the vocal map says is really sung, next to the vocal stem's
//! level, to check the leakage detection by eye (and ear, at the printed times).
//!
//! `cargo run --release -p clubcraft-analysis --example vocal_probe -- <library.db> <name substring>`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // developer tool: failing loudly is fine

use clubcraft_analysis::{vocals, OwnedStems};
use clubcraft_core::library::stems_path;
use clubcraft_core::Library;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let lib = Library::open(args[1].as_ref()).unwrap();
    let pat = args[2].to_lowercase();
    for t in lib.tracks().unwrap().into_iter().filter(|t| t.display_name().to_lowercase().contains(&pat)) {
        let Some(a) = lib.analysis(t.id).unwrap() else { continue };
        let (Some(g), true) = (a.grid, stems_path(&t.hash).exists()) else { continue };
        let st = clubcraft_stems::load_stem_track(&stems_path(&t.hash)).unwrap();
        let owned = OwnedStems::from_stem_track_partial(&st);
        drop(st);
        let t0 = Instant::now();
        let m = vocals::compute(&owned.as_ref(), g, a.duration);
        let fs = vocals::frame_stats(&owned.as_ref());
        let loud = {
            let mut l: Vec<f32> = fs.iter().map(|f| f[0]).filter(|x| *x > -110.0).collect();
            l.sort_by(|a, b| a.total_cmp(b));
            l[(l.len() as f32 * 0.95) as usize]
        };
        let fstats = |bar: i32| {
            let (t0, t1) = (g.time_of_bar(bar as f64), g.time_of_bar(bar as f64 + 1.0));
            let r = ((t0.max(0.0) * 44100.0 / 512.0) as usize).min(fs.len())..((t1 * 44100.0 / 512.0) as usize).min(fs.len());
            let n = r.len().max(1) as f32;
            let mut lv: Vec<f32> = fs[r.clone()].iter().map(|f| f[0] - loud).collect();
            lv.sort_by(|a, b| a.total_cmp(b));
            let med = |i: usize| { let mut v: Vec<f32> = fs[r.clone()].iter().map(|f| f[i]).collect(); v.sort_by(|a, b| a.total_cmp(b)); v.get(v.len() / 2).copied().unwrap_or(0.0) };
            let _ = n;
            format!("lvl p10 {:>5.0} p50 {:>5.0}  other cos {:.2} gap {:>5.1}  drums cos {:.2} gap {:>5.1}", lv.get(lv.len() / 10).copied().unwrap_or(-99.0), lv.get(lv.len() / 2).copied().unwrap_or(-99.0), med(1), med(2), med(3), med(4))
        };
        println!("\n== {}  ({:.2} s)", t.display_name(), t0.elapsed().as_secs_f32());
        let nb = g.bar_at(a.duration).floor() as i32;
        // the structure model's functional labels per bar (ALLIN1=1, ~30 s per track)
        let act = if std::env::var("ALLIN1").is_ok() { clubcraft_analysis::allinone::shared_activations(&owned.as_ref()) } else { None };
        let label = |bar: i32| -> String {
            let Some(act) = &act else { return String::new() };
            let l = clubcraft_analysis::allinone::mean_labels(act, g.time_of_bar(bar as f64), g.time_of_bar(bar as f64 + 1.0));
            let (i, p) = l.iter().enumerate().max_by(|x, y| x.1.total_cmp(y.1)).unwrap();
            format!("{:<7} {:>3.0}%  chorus {:>3.0}%", clubcraft_analysis::allinone::LABELS[i], p * 100.0, l[9] * 100.0)
        };
        for bar in 0..nb {
            let kind = a.sections.iter().find(|q| bar >= q.start_bar && bar < q.start_bar + q.bars as i32).map(|q| q.kind.label()).unwrap_or("?");
            let s = m.step_of_bar(bar);
            // breaks between lines blank, singing by level
            let line: String = (s..s + m.steps_per_bar()).map(|k| if m.is_break(k) { ' ' } else { match m.level_at(k) { x if x > -6.0 => '█', x if x > -12.0 => '▓', x if x > -20.0 => '▒', _ => '·' } }).collect();
            let sec = g.time_of_bar(bar as f64);
            let stats = if std::env::var("STATS").is_ok() { fstats(bar) } else { String::new() };
            println!("{:>4} {:>2}:{:02} {:<6} {:>4.0}% |{line}| {} {stats}", bar + 1, (sec / 60.0) as i32, (sec % 60.0) as i32, kind, 100.0 * m.bar_sung(bar), label(bar));
        }
        let mut ph: Vec<(i32, vocals::PhraseScore)> = (0..nb - 16).step_by(4).map(|b| (b, m.phrase(b, 16))).collect();
        ph.sort_by(|a, b| b.1.quality.total_cmp(&a.1.quality));
        for (b, p) in ph.iter().take(10) {
            let sec = g.time_of_bar(*b as f64);
            println!("  bar {:>3} ({}:{:02}) q {:.2}  cover {:.2} start {:.2} end {:.2} lyrical {:.2} hook {:.2}", b + 1, (sec / 60.0) as i32, (sec % 60.0) as i32, p.quality, p.cover, p.start, p.end, p.lyrical, p.hook);
        }
    }
}
