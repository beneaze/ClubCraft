//! Calibrate / sanity-check the mashup scorer on a library with stems.
//!
//! `CLUBCRAFT_HOME=<dir> cargo run --release -p clubcraft-analysis --example mashup_eval`
//!
//! For every track with stems: computes the per-bar features (timing them), then compares the
//! harmony of each vocal phrase over its *own* instrumental (a known-good pairing) with the best
//! and the median pairing over the other tracks' instrumentals.

use clubcraft_analysis::mashup::{self, MashupFeatures, MatchOptions};
use clubcraft_analysis::OwnedStems;
use clubcraft_core::library::stems_path;
use clubcraft_core::Library;
use std::time::Instant;

fn main() {
    let lib = Library::open_default().expect("library");
    let analyses = lib.all_analyses().expect("analyses");
    let mut feats: Vec<(i64, String, MashupFeatures)> = Vec::new();
    let learned = std::env::var("COCOLA").is_ok();
    let mut audio16: std::collections::HashMap<i64, (Vec<f32>, Vec<f32>)> = Default::default();
    for t in lib.tracks().unwrap() {
        let Some(a) = analyses.get(&t.id) else { continue };
        let (Some(grid), true) = (a.grid, stems_path(&t.hash).exists()) else { continue };
        let t0 = Instant::now();
        let st = clubcraft_stems::load_stem_track(&stems_path(&t.hash)).unwrap();
        let owned = OwnedStems::from_stem_track_partial(&st);
        drop(st);
        let load = t0.elapsed().as_secs_f32();
        let starts: Vec<f64> = a.sections.iter().map(|s| s.start).collect();
        let t1 = Instant::now();
        let g2 = if std::env::var("BEATS").is_ok() { clubcraft_core::BeatGrid { beats_per_bar: 1, ..grid } } else { grid };
        let f = mashup::compute(&owned.as_ref(), g2, a.duration, a.gain_db.unwrap_or(0.0), &starts);
        if learned {
            let r = owned.as_ref();
            let v = clubcraft_audio::resample::resample_channel(&r.mono(clubcraft_core::StemKind::Vocals), 44_100, 16_000);
            let i = clubcraft_audio::resample::resample_channel(&r.mix_mono([1.0, 1.0, 1.0, 0.0], r.frames()), 44_100, 16_000);
            audio16.insert(t.id, (v, i));
        }
        println!("{:>4} {:<40} {:>4} bars  load {load:.1}s  features {:.2}s", t.id, t.display_name().chars().take(40).collect::<String>(), f.bars.len(), t1.elapsed().as_secs_f32());
        feats.push((t.id, t.display_name(), f));
    }
    if std::env::var("DUMP").is_ok() {
        for (_, name, f) in feats.iter().take(3) {
            println!("{name}: key {:?}", analyses.values().find(|_| false).map(|_: &clubcraft_core::Analysis| 0));
            let r = mashup::vocal_regions(f, &MatchOptions::default(), 1);
            dump(f, r.first().copied().unwrap_or(16), 6);
        }
        return;
    }
    let beats = std::env::var("BEATS").is_ok();
    let (plen, pstep) = if beats { (64i32, 16i32) } else { (16, 4) };
    let o = MatchOptions { key_range: 0, tempo_tolerance: 0.5, bars: plen as usize, step_bars: pstep, ..Default::default() };
    let avg = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
    let (mut own, mut shifted, mut offset, mut random, mut best) = (vec![], vec![], vec![], vec![], vec![]);
    let (mut beats_shift, mut beats_random, mut n) = (0, 0, 0);
    for (id, _name, f) in &feats {
        for r in mashup::vocal_regions(f, &o, 3) {
            let Some(own_m) = mashup::score(f, r, f, r, 0, &o) else { continue };
            n += 1;
            own.push(own_m.harmony);
            let sh: Vec<f32> = [-2, -1, 1, 2].iter().filter_map(|&k| mashup::score(f, r, f, r, k, &o)).map(|m| m.harmony).collect();
            if own_m.harmony > sh.iter().cloned().fold(0.0, f32::max) {
                beats_shift += 1;
            }
            shifted.extend(sh);
            offset.extend([-2, -1, 1, 2].iter().filter_map(|&d| mashup::score(f, r, f, r + d, 0, &o)).map(|m| m.harmony));
            let mut rnd = vec![];
            let mut bst = 0f32;
            for (j, _, g) in &feats {
                if j == id {
                    continue;
                }
                let mut b = g.first_bar.div_euclid(4) * 4;
                while b + o.bars as i32 <= g.last_bar() {
                    if let Some(m) = mashup::score(f, r, g, b, 0, &o) {
                        rnd.push(m.harmony);
                        bst = bst.max(m.harmony);
                    }
                    b += 4;
                }
            }
            if own_m.harmony > avg(&rnd) {
                beats_random += 1;
            }
            random.extend(rnd);
            best.push(bst);
        }
    }
    println!("\nphrases {n}");
    println!("harmony  own {:.3} | own ±1-2 semitones {:.3} | own ±1-2 bars {:.3} | random placements {:.3} | best other {:.3}", avg(&own), avg(&shifted), avg(&offset), avg(&random), avg(&best));
    println!("own beats all its key shifts in {beats_shift}/{n}, beats the random mean in {beats_random}/{n}");
    let mut s = own.clone();
    s.sort_by(|a, b| a.total_cmp(b));
    let mut rr = random.clone();
    rr.sort_by(|a, b| a.total_cmp(b));
    let q = |v: &[f32], p: f32| v.get(((v.len() as f32 - 1.0) * p) as usize).copied().unwrap_or(0.0);
    println!("own p10 {:.2} p50 {:.2} p90 {:.2} | random p10 {:.2} p50 {:.2} p90 {:.2}", q(&s, 0.1), q(&s, 0.5), q(&s, 0.9), q(&rr, 0.1), q(&rr, 0.5), q(&rr, 0.9));
    if learned {
        cocola_eval(&feats, &audio16);
        return;
    }
    // AUC of scoring variants: P(own aligned pair > random placement), vocal-weighted bars
    let variants: Vec<(&str, [f32; 12], f32)> = vec![
        ("current", mashup::CONSONANCE, 0.45),
        ("dissonance only", [0.0, -1.0, 0.0, 0.0, 0.0, 0.0, -0.6, 0.0, -0.2, 0.0, 0.0, -0.1], 0.45),
        ("unison (cosine-like)", [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 0.45),
        ("chord tones", [1.0, -1.0, 0.0, 0.6, 0.6, 0.0, -0.8, 0.8, -0.3, 0.3, 0.0, -0.3], 0.45),
        ("current, bass only", mashup::CONSONANCE, 1.0),
        ("current, melody only", mashup::CONSONANCE, 0.0),
    ];
    for (label, table, bw) in &variants {
        let phrase_score = |f: &MashupFeatures, r: i32, g: &MashupFeatures, b: i32, k: i32| -> Option<f32> {
            let (mut a, mut w) = (0f32, 0f32);
            for j in 0..plen {
                let (Some(vb), Some(ib)) = (f.bar(r + j), g.bar(b + j)) else { return None };
                if vb.vocal_db < -45.0 {
                    continue;
                }
                let mut rot = [0f32; 12];
                for p in 0..12 {
                    rot[(p as i32 + k).rem_euclid(12) as usize] = vb.vocal[p];
                }
                if let Some(c) = mashup::bar_consonance(&rot, ib, table, *bw) {
                    a += c;
                    w += 1.0;
                }
            }
            (w >= plen as f32 / 4.0).then(|| a / w)
        };
        let (mut auc, mut auc_k, mut n) = (0f32, 0f32, 0);
        for (id, _, f) in &feats {
            for r in mashup::vocal_regions(f, &o, 3) {
                let Some(own) = phrase_score(f, r, f, r, 0) else { continue };
                let (mut below, mut tot) = (0, 0);
                for (j, _, g) in &feats {
                    if j == id {
                        continue;
                    }
                    let mut b = g.first_bar.div_euclid(pstep) * pstep;
                    while b + plen <= g.last_bar() {
                        if let Some(x) = phrase_score(f, r, g, b, 0) {
                            tot += 1;
                            below += (x < own) as i32;
                        }
                        b += pstep;
                    }
                }
                let ks: Vec<f32> = [-2, -1, 1, 2].iter().filter_map(|&k| phrase_score(f, r, f, r, k)).collect();
                auc_k += ks.iter().filter(|&&x| x < own).count() as f32 / ks.len().max(1) as f32;
                auc += below as f32 / tot.max(1) as f32;
                n += 1;
            }
        }
        println!("{label:<24} AUC vs random {:.3}  vs key shifts {:.3}", auc / n as f32, auc_k / n as f32);
    }
    // full search timing
    if let Some((id, _, f)) = feats.first() {
        let o = MatchOptions::default();
        let regions = mashup::vocal_regions(f, &o, 3);
        let cands: Vec<(i64, &MashupFeatures)> = feats.iter().map(|(i, _, g)| (*i, g)).collect();
        let t = Instant::now();
        let r = mashup::find((*id, f), &regions, &cands, &o);
        let name = |i: i64| feats.iter().find(|x| x.0 == i).map(|x| x.1.chars().take(30).collect::<String>()).unwrap_or_default();
        for m in r.iter().take(8) {
            println!("  {:<30} vocal bar {:>3} → {:<30} bar {:>3} key {:+} tempo {:+.1}% score {:.2} (harm {:.2} min {:.2} mask {:.2} resid {:.2})", name(m.vocal_track), m.vocal_bar, name(m.inst_track), m.inst_bar, m.key_shift, (m.tempo_ratio - 1.0) * 100.0, m.score, m.harmony, m.harmony_min, m.masking, m.residual_vocals);
        }
        println!("search over {} tracks: {:.1} ms, {} matches, top {:?}", cands.len(), t.elapsed().as_secs_f64() * 1000.0, r.len(), r.first().map(|m| (m.inst_track, m.inst_bar, m.key_shift, m.score)));
    }
}

#[allow(dead_code)]
pub fn dump(f: &MashupFeatures, from: i32, n: i32) {
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let top = |c: &[f32; 12]| {
        let mut idx: Vec<usize> = (0..12).filter(|&i| c[i] > 0.0).collect();
        idx.sort_by(|a, b| c[*b].total_cmp(&c[*a]));
        idx.iter().take(4).map(|&i| format!("{}{:.0}", N[i], c[i] * 100.0)).collect::<Vec<_>>().join(" ")
    };
    for b in from..from + n {
        if let Some(x) = f.bar(b) {
            println!("  bar {b:>3} V[{:>5.1}dB] {:<24} B[{:>5.1}] {:<22} O[{:>5.1}] {:<24}", x.vocal_db, top(&x.vocal), x.bass_db, top(&x.bass), x.other_db, top(&x.other));
        }
    }
}

fn cocola_eval(feats: &[(i64, String, MashupFeatures)], audio: &std::collections::HashMap<i64, (Vec<f32>, Vec<f32>)>) {
    use clubcraft_analysis::cocola;
    let m = cocola::Cocola::load_default().expect("cocola model");
    let o = MatchOptions { key_range: 0, tempo_tolerance: 0.5, ..Default::default() };
    let clip = |x: &[f32], t: f64| -> Vec<f32> {
        let s = (t.max(0.0) * 16000.0) as usize;
        let mut c: Vec<f32> = x.get(s..(s + cocola::CLIP).min(x.len())).unwrap_or(&[]).to_vec();
        c.resize(cocola::CLIP, 0.0);
        c
    };
    let mut rng = 12345u64;
    let mut rand = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % n as u64) as usize
    };
    let (mut auc_rule, mut auc_l, mut auc_mix) = (0f32, 0f32, [0f32; 5]);
    let weights = [0.2f32, 0.35, 0.5, 0.65, 0.8];
    let mut n = 0;
    let t0 = Instant::now();
    for (id, _, f) in feats {
        let (va, _) = &audio[id];
        for r in mashup::vocal_regions(f, &o, 3) {
            // two 5 s windows on the loudest vocal bar of each half of the phrase
            let pick = |a: i32, b: i32| (a..b).max_by(|x, y| f.bar(r + x).map(|q| q.vocal_db).unwrap_or(-99.0).total_cmp(&f.bar(r + y).map(|q| q.vocal_db).unwrap_or(-99.0))).unwrap_or(a);
            let js = [pick(0, 8), pick(8, 14)];
            let vclips: Vec<Vec<f32>> = js.iter().map(|&j| cocola::features(&clip(va, f.bar_time(r + j)))).collect();
            let ve = m.embed(&vclips, false).unwrap();
            // own placement + 20 random placements in other tracks
            let mut placements: Vec<(i64, i32)> = vec![(*id, r)];
            for _ in 0..20 {
                let (j, _, g) = &feats[rand(feats.len())];
                if j == id || g.bars.len() < 20 {
                    continue;
                }
                let b = g.first_bar + (rand(g.bars.len() - 16) as i32 / 4) * 4;
                placements.push((*j, b));
            }
            let mut rows: Vec<(f32, f32)> = Vec::new(); // (rule, learned)
            for (j, b) in &placements {
                let g = &feats.iter().find(|x| x.0 == *j).unwrap().2;
                let Some(rule) = mashup::score(f, r, g, *b, 0, &o) else { continue };
                let (_, ib) = &audio[j];
                let ic: Vec<Vec<f32>> = js.iter().map(|&k| cocola::features(&clip(ib, g.bar_time(b + k)))).collect();
                let ie = m.embed(&ic, false).unwrap();
                let l = (0..2).map(|k| cocola::normalized(m.score(&ve[k], &ie[k]))).sum::<f32>() / 2.0;
                rows.push((rule.score, l));
            }
            if rows.len() < 5 || placements[0].0 != *id {
                continue;
            }
            let own = rows[0];
            let auc = |f: &dyn Fn(&(f32, f32)) -> f32| rows[1..].iter().filter(|x| f(x) < f(&own)).count() as f32 / (rows.len() - 1) as f32;
            auc_rule += auc(&|x| x.0);
            auc_l += auc(&|x| x.1);
            for (k, w) in weights.iter().enumerate() {
                auc_mix[k] += auc(&|x| (1.0 - w) * x.0 + w * x.1);
            }
            n += 1;
        }
    }
    let n = n as f32;
    println!("\nCOCOLA eval over {n} phrases ({:.0} s): AUC own-vs-random  rule {:.3}  learned {:.3}", t0.elapsed().as_secs_f32(), auc_rule / n, auc_l / n);
    for (k, w) in weights.iter().enumerate() {
        println!("  blend {:.2} learned: {:.3}", w, auc_mix[k] / n);
    }
}
