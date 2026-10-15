//! Automatic hot cues.
//!
//! Up to 8 [`CueSource::Auto`](clubcraft_core::CueSource::Auto) cues in stable pad slots, so
//! the same pad always means the same thing in the live decks. Every cue sits on a downbeat
//! (bar line) of the grid; beat 0 of the grid is the first downbeat with music.
//!
//! | slot | kind | name | position |
//! |---|---|---|---|
//! | 0 | `FirstBeat` | Start | first downbeat with music (grid bar 0) |
//! | 1 | `MixIn` | Mix In | 8-bar line where the drums are established, at least 32 beats before the first drop and the first vocal (else the start) |
//! | 2 | `VocalIn` | Vocal | first vocal entry snapped to its bar / 4-bar line (stems required) |
//! | 3 | `Buildup` / `Breakdown` | Build / Break | the build (or break) leading into the first drop |
//! | 4 | `Drop` | Drop | first drop, on its downbeat |
//! | 5 | `Breakdown` | Break | main breakdown: the longest one after the first drop |
//! | 6 | `Drop` / `Outro` | Drop / Outro | the drop after the main breakdown, otherwise the outro |
//! | 7 | `MixOut` | Mix Out | start of the outro, otherwise 64 (short tracks: 32) beats before the end of the music |
//!
//! Cues that do not exist are skipped (the slot stays empty). Further drops / breakdowns /
//! builds / the outro are returned as slot-less markers, as are "Phrase" markers (kind `Hot`)
//! at phrase changes inside a section (where two equally labelled sections were merged). `MixIn` and `MixOut` are on 16- or
//! 32-beat phrase lines from the anchor, because the studio aligns the incoming track's mix-in
//! with the outgoing track's mix-out. Without structure, mix-in is the first 32-beat line at
//! least 15 s into the track and mix-out the last 32-beat line at least 30 s before the end.

use crate::structure::BarFeatures;
use clubcraft_core::{BeatGrid, Cue, CueKind, Section, SectionKind};

const PHRASE: f64 = 32.0;

/// Generate cues from a grid and sections, optionally refined with bar features (music end,
/// drum and vocal entry). See the module docs for the slot layout.
pub fn generate(
    grid: &BeatGrid,
    sections: &[Section],
    bars: Option<&BarFeatures>,
    duration: f64,
) -> Vec<Cue> {
    match bars {
        Some(b) => generate_from(grid, sections, b, &[], duration),
        None => generate_from(grid, sections, &BarFeatures::default(), &[], duration),
    }
}

fn floor_to(b: f64, q: f64) -> f64 {
    (b / q).floor() * q
}
fn round_to(b: f64, q: f64) -> f64 {
    (b / q).round() * q
}

/// A run of sections with the same label (sections are merged, so normally one section).
struct Run {
    kind: SectionKind,
    /// Start beat, snapped to the bar.
    beat: f64,
    bars: u32,
}

pub(crate) fn generate_from(
    grid: &BeatGrid,
    sections: &[Section],
    bars: &BarFeatures,
    phrases: &[f64],
    duration: f64,
) -> Vec<Cue> {
    let bpb = grid.beats_per_bar as f64;
    let at_beat = |b: f64| grid.time_of_beat(b);
    let bar_beat = |t: f64| round_to(grid.beat_at(t), bpb);
    let mut cues: Vec<Cue> = Vec::new();
    let mut push = |slot: Option<u8>, kind: CueKind, beat: f64| {
        let t = at_beat(beat);
        if t >= -0.03 && t < duration - 0.5 {
            cues.push(Cue::auto(slot, kind, t.max(0.0)));
        }
    };
    let has_bars = !bars.is_empty();

    // End of the music: the end of the last bar above -40 dB (trailing silence ignored).
    let total_beats = grid.beat_at(duration).floor();
    let music_end = if has_bars {
        (0..bars.len())
            .rev()
            .find(|&i| bars.level_db[i] > -40.0)
            .map(|i| ((bars.bar_index(i) + 1) as f64 * bpb).min(total_beats))
            .unwrap_or(total_beats)
    } else {
        total_beats
    };

    // --- slot 0: first downbeat with music = the grid anchor.
    push(Some(0), CueKind::FirstBeat, 0.0);

    // Structure runs.
    let mut runs: Vec<Run> = Vec::new();
    for s in sections {
        match runs.last_mut() {
            Some(r) if r.kind == s.kind => r.bars += s.bars,
            _ => runs.push(Run {
                kind: s.kind,
                beat: bar_beat(s.start).max(0.0),
                bars: s.bars,
            }),
        }
    }
    let first_drop = runs.iter().position(|r| r.kind == SectionKind::Drop);
    let first_drop_beat = first_drop.map(|i| runs[i].beat);

    // --- slot 2: vocal entry (computed first: it limits the mix-in).
    let mut vocal_beat: Option<f64> = None;
    if bars.has_stems && has_bars {
        let n = bars.len();
        let v = |i: usize| bars.stems[i][3];
        if let Some(mut i) = (0..n.saturating_sub(1))
            .find(|&i| bars.start[i] >= -0.03 && v(i) > 0.5 && v(i + 1) > 0.4)
        {
            // Snap to the 4-bar grid when within a bar: a vocal in the last bar of a group is a
            // pickup into the next phrase; one a bar late belongs to the line before it.
            match bars.bar_index(i).rem_euclid(4) {
                3 if i + 1 < n => i += 1,
                1 if i > 0 && bars.start[i - 1] >= -0.03 => i -= 1,
                _ => {}
            }
            vocal_beat = Some(bars.bar_index(i) as f64 * bpb);
        }
    }

    // --- slot 1: mix-in. The 8-bar line where the drums are established, at least 32 beats
    // before the first drop and the first vocal so the blend happens over intro material.
    // Without structure: the first phrase line at least 15 s in (0 when that is past 40 % of the
    // track), the last phrase line at least 30 s before the end (ClubCraft's own fallback).
    let beats_per_sec = total_beats / duration.max(1.0);
    let mix_in = if !has_bars {
        let m = (15.0 * beats_per_sec / PHRASE).ceil() * PHRASE;
        if at_beat(m) > duration * 0.4 {
            0.0
        } else {
            m
        }
    } else {
        let limit = [first_drop_beat, vocal_beat]
            .iter()
            .flatten()
            .fold(f64::INFINITY, |a, b| a.min(*b))
            - PHRASE;
        let drums_at = (0..bars.len().saturating_sub(1))
            .find(|&i| {
                bars.start[i] >= -0.03 && bars.stems[i][0] > 0.5 && bars.stems[i + 1][0] > 0.5
            })
            .map(|i| bars.bar_index(i).max(0) as f64 * bpb);
        let mut m = match drums_at {
            // Drums entering up to 2 bars after a phrase line count from that line.
            Some(b) if b - floor_to(b, PHRASE) <= 2.0 * bpb => floor_to(b, PHRASE),
            Some(b) => floor_to(b, PHRASE) + PHRASE,
            None => 0.0,
        };
        if m > limit {
            m = floor_to(limit.max(0.0), PHRASE);
        }
        if at_beat(m) > duration * 0.4 {
            m = 0.0;
        }
        m.max(0.0)
    };
    push(Some(1), CueKind::MixIn, mix_in);
    if let Some(vb) = vocal_beat {
        push(Some(2), CueKind::VocalIn, vb);
    }

    // --- slots 3..6 from the structure runs.
    let mut used = vec![false; runs.len()];
    if let Some(fd) = first_drop {
        if fd > 0 {
            let p = &runs[fd - 1];
            let kind = match p.kind {
                SectionKind::Buildup => Some(CueKind::Buildup),
                SectionKind::Breakdown => Some(CueKind::Breakdown),
                _ => None,
            };
            if let Some(k) = kind {
                push(Some(3), k, p.beat);
                used[fd - 1] = true;
            }
        }
        push(Some(4), CueKind::Drop, runs[fd].beat);
        used[fd] = true;
    }
    let after = first_drop.map(|i| i + 1).unwrap_or(0);
    let main_break = (after..runs.len())
        .filter(|&i| runs[i].kind == SectionKind::Breakdown && !used[i])
        .max_by_key(|&i| (runs[i].bars, usize::MAX - i));
    let next_drop = |from: usize, used: &[bool]| {
        (from..runs.len()).find(|&i| runs[i].kind == SectionKind::Drop && !used[i])
    };
    let mut second_drop = None;
    if let Some(mb) = main_break {
        push(Some(5), CueKind::Breakdown, runs[mb].beat);
        used[mb] = true;
        second_drop = next_drop(mb + 1, &used);
    }
    if second_drop.is_none() {
        second_drop = first_drop.and_then(|fd| next_drop(fd + 1, &used));
    }
    let outro = runs
        .iter()
        .position(|r| r.kind == SectionKind::Outro && at_beat(r.beat) > duration * 0.5);
    match second_drop {
        Some(sd) => {
            push(Some(6), CueKind::Drop, runs[sd].beat);
            used[sd] = true;
        }
        None => {
            if let Some(o) = outro {
                push(Some(6), CueKind::Outro, runs[o].beat);
                used[o] = true;
            }
        }
    }

    // --- slot 7: mix-out: start of the outro (on a 16-beat line when within a bar), else
    // 64 / 32 beats before the end of the music.
    let end_line = round_to(music_end, 16.0);
    let mut mix_out = if !has_bars {
        let m = floor_to(total_beats - 30.0 * beats_per_sec, PHRASE);
        if m > mix_in {
            m
        } else {
            floor_to(total_beats - 16.0, 16.0)
        }
    } else if end_line - 64.0 >= mix_in + 4.0 * PHRASE {
        end_line - 64.0
    } else {
        end_line - 32.0
    };
    if let Some(o) = outro {
        let ob = runs[o].beat;
        let bars_left = (music_end - ob) / bpb;
        if (8.0..=64.0).contains(&bars_left) {
            let l16 = round_to(ob, 16.0);
            mix_out = if (l16 - ob).abs() <= bpb { l16 } else { ob };
        }
    }
    if mix_out <= mix_in {
        mix_out = floor_to((music_end - 32.0).max(mix_in + 16.0), 16.0);
    }
    push(Some(7), CueKind::MixOut, mix_out);

    // --- slot-less markers for the remaining structure.
    for (i, r) in runs.iter().enumerate() {
        if used[i] {
            continue;
        }
        let kind = match r.kind {
            SectionKind::Drop => CueKind::Drop,
            SectionKind::Breakdown if r.bars >= 8 => CueKind::Breakdown,
            SectionKind::Outro if at_beat(r.beat) > duration * 0.5 => CueKind::Outro,
            SectionKind::Buildup => CueKind::Buildup,
            _ => continue,
        };
        push(None, kind, r.beat);
    }
    // Phrase changes inside a merged section (e.g. vocal drop -> instrumental drop).
    for &t in phrases {
        let b = bar_beat(t);
        let t = at_beat(b);
        // Only phrase lines (8 bars) and not crowding other cues.
        let crowded = cues
            .iter()
            .any(|c| (c.pos - t).abs() < 8.0 * grid.bar_len() * 0.99);
        if (b / PHRASE).fract().abs() < 1e-6 && !crowded && t >= 0.0 && t < duration - 0.5 {
            let mut c = Cue::auto(None, CueKind::Hot, t);
            c.name = "Phrase".into();
            cues.push(c);
        }
    }
    cues.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    // Drop slot-less markers at (or within a bar of) another cue; slotted cues win.
    let mut kept: Vec<Cue> = Vec::new();
    for c in cues.iter().filter(|c| c.slot.is_some()) {
        kept.push(c.clone());
    }
    for c in cues.iter().filter(|c| c.slot.is_none()) {
        if !kept
            .iter()
            .any(|k| (k.pos - c.pos).abs() < grid.bar_len() * 0.99)
        {
            kept.push(c.clone());
        }
    }
    kept.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    kept
}
