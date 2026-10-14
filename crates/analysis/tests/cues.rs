//! Cue rules on a hand-built extended-mix structure.

use clubcraft_analysis::cues;
use clubcraft_analysis::structure::BarFeatures;
use clubcraft_core::{BeatGrid, CueKind, Section, SectionKind};

/// 124 BPM extended mix: intro 32 bars (drums from bar 0, vocal from bar 24), drop 32, break 16,
/// build 8, drop 32, outro 24, then 2 bars of silence.
fn track() -> (BeatGrid, Vec<Section>, BarFeatures, f64) {
    let g = BeatGrid::new(124.0, 0.12);
    let layout = [
        (SectionKind::Intro, 32, [1.0, 0.0, 0.3, 0.0]),
        (SectionKind::Drop, 32, [1.0, 1.0, 1.0, 1.0]),
        (SectionKind::Breakdown, 16, [0.0, 0.0, 1.0, 1.0]),
        (SectionKind::Buildup, 8, [0.8, 0.0, 1.0, 0.5]),
        (SectionKind::Drop, 32, [1.0, 1.0, 1.0, 1.0]),
        (SectionKind::Outro, 24, [1.0, 0.0, 0.0, 0.0]),
    ];
    let mut sections = Vec::new();
    let mut bars = BarFeatures { has_stems: true, ..Default::default() };
    let mut bar = 0i32;
    for (kind, n, stems) in layout {
        sections.push(Section {
            kind,
            start: g.time_of_bar(bar as f64),
            end: g.time_of_bar((bar + n) as f64),
            start_bar: bar,
            bars: n as u32,
            energy: 0.8,
            stems,
        });
        for i in 0..n {
            let mut s = stems;
            if kind == SectionKind::Intro {
                s[3] = if bar + i >= 24 { 1.0 } else { 0.0 };
            }
            bars.start.push(g.time_of_bar((bar + i) as f64));
            bars.stems.push(s);
            bars.level_db.push(-3.0);
            bars.bands_db.push([0.0; 4]);
            bars.density.push(1.0);
            bars.timbre.push([0.0; 8]);
            bars.energy.push(0.8);
        }
        bar += n;
    }
    for _ in 0..2 {
        bars.start.push(g.time_of_bar(bar as f64));
        bars.stems.push([0.0; 4]);
        bars.level_db.push(-60.0);
        bars.bands_db.push([-60.0; 4]);
        bars.density.push(0.0);
        bars.timbre.push([-4.0; 8]);
        bars.energy.push(0.0);
        bar += 1;
    }
    let duration = g.time_of_bar(bar as f64);
    sections.last_mut().unwrap().end = duration;
    (g, sections, bars, duration)
}

#[test]
fn dj_cue_rules() {
    let (g, sections, bars, duration) = track();
    let c = cues::generate(&g, &sections, Some(&bars), duration);
    let beat = |k: CueKind| g.beat_at(c.iter().find(|x| x.kind == k && x.slot.is_some()).unwrap_or_else(|| panic!("{k:?}")).pos);
    let slot = |s: u8| c.iter().find(|x| x.slot == Some(s)).map(|x| (x.kind, g.beat_at(x.pos)));
    assert!((beat(CueKind::FirstBeat)).abs() < 1e-6);
    // Mix-in: drums from bar 0, but the vocal enters at beat 96 and the drop at 128 -> the
    // mix-in must be at least 32 beats before both: beat 0..64 on an 8-bar line.
    let mi = beat(CueKind::MixIn);
    assert!(mi <= 96.0 - 32.0 && (mi / 32.0).fract().abs() < 1e-6, "mix-in {mi}");
    assert_eq!(slot(2).map(|s| s.1.round()), Some(96.0));
    // The first drop follows the intro directly: no build / break cue before it.
    assert!(slot(3).is_none());
    // The build before the second drop is a marker.
    assert!(c.iter().any(|x| x.slot.is_none() && x.kind == CueKind::Buildup && (g.beat_at(x.pos) - 320.0).abs() < 1e-6));
    // Drops exactly on their downbeats.
    assert_eq!(slot(4), Some((CueKind::Drop, 128.0)));
    assert_eq!(slot(5).map(|s| s.0), Some(CueKind::Breakdown));
    assert_eq!(slot(5).map(|s| s.1.round()), Some(256.0));
    assert_eq!(slot(6).map(|s| (s.0, s.1.round())), Some((CueKind::Drop, 352.0)));
    // Mix-out at the start of the outro.
    assert_eq!(slot(7).map(|s| (s.0, s.1.round())), Some((CueKind::MixOut, 480.0)));
    for x in &c {
        let b = g.beat_at(x.pos);
        assert!((b / 4.0 - (b / 4.0).round()).abs() < 1e-6, "{:?} not on a downbeat: {b}", x.kind);
        assert!(x.name.len() <= 8, "{}", x.name);
    }
}

#[test]
fn mix_out_without_outro_counts_from_the_end_of_the_music() {
    let (g, mut sections, bars, duration) = track();
    // Turn the outro into a drop: no outro section.
    let n = sections.len();
    sections[n - 1].kind = SectionKind::Drop;
    let c = cues::generate(&g, &sections, Some(&bars), duration);
    let mo = c.iter().find(|x| x.kind == CueKind::MixOut).unwrap();
    // Music ends at bar 144 (beat 576): mix-out 64 beats earlier.
    assert!((g.beat_at(mo.pos) - 512.0).abs() < 1e-6, "{}", g.beat_at(mo.pos));
}
