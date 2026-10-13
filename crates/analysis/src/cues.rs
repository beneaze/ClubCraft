//! Automatic hot cues.
//!
//! Up to 8 beat-snapped [`CueSource::Auto`](clubcraft_core::CueSource::Auto) cues in stable pad slots, so that the same pad
//! always means the same thing in the live decks:
//!
//! | slot | kind | position |
//! |---|---|---|
//! | 0 | `FirstBeat` | first downbeat with music |
//! | 1 | `MixIn` | phrase-aligned start of the mix-in region (first 8-bar line at/before the drums enter) |
//! | 2 | `VocalIn` | first vocal entry snapped to its bar (stems required) |
//! | 3 | `Buildup` / `Breakdown` | the build (or break) leading into the first drop |
//! | 4 | `Drop` | first drop |
//! | 5 | `Breakdown` | main breakdown (longest one after the first drop) |
//! | 6 | `Drop` / `Outro` | second drop (after the main break), otherwise the outro |
//! | 7 | `MixOut` | phrase-aligned start of the outro, or 32 / 16 bars before the end |
//!
//! Cues that do not exist are skipped (the slot stays empty). Additional drops / breakdowns /
//! the outro are returned as slot-less markers. `MixIn` and `MixOut` are always a multiple of
//! 32 beats (8 bars) from the grid's downbeat, because the studio aligns the incoming track's
//! mix-in with the outgoing track's mix-out. Fallback when no structure is available follows
//! DJ.Studio's defaults (mix-in at beat 32 / mix-out at the last multiple of 32 beats at
//! least 32 beats before the end for tracks longer than 3 minutes, else beat 0 / end − 16).

use crate::structure::BarFeatures;
use clubcraft_core::{BeatGrid, Cue, CueKind, Section, SectionKind};

const PHRASE_BEATS: f64 = 32.0;

/// Generate cues from a grid and sections, optionally refined with bar features (music start,
/// drum and vocal entry). See the module docs for the slot layout.
pub fn generate(
    grid: &BeatGrid,
    sections: &[Section],
    bars: Option<&BarFeatures>,
    duration: f64,
) -> Vec<Cue> {
    match bars {
        Some(b) => generate_from(grid, sections, b, duration),
        None => generate_from(grid, sections, &BarFeatures::default(), duration),
    }
}

pub(crate) fn generate_from(
    grid: &BeatGrid,
    sections: &[Section],
    bars: &BarFeatures,
    duration: f64,
) -> Vec<Cue> {
    let bpb = grid.beats_per_bar as f64;
    let total_beats = grid.beat_at(duration).floor();
    let at_beat = |b: f64| grid.time_of_beat(b);
    let snap_beat = |t: f64| grid.beat_at(t).round();
    // Phrase arithmetic is relative to the phrase origin (first bar with music).
    let ob = if bars.is_empty() {
        0.0
    } else {
        bars.phrase_origin as f64 * bpb
    };
    let pfloor = |b: f64, q: f64| ob + ((b - ob) / q).floor() * q;
    let pround = |b: f64, q: f64| ob + ((b - ob) / q).round() * q;
    let mut cues: Vec<Cue> = Vec::new();
    let mut push = |slot: Option<u8>, kind: CueKind, beat: f64| {
        let t = at_beat(beat);
        if t >= -0.03 && t < duration - 0.5 {
            cues.push(Cue::auto(slot, kind, t.max(0.0)));
        }
    };

    // --- slot 0: first downbeat with music.
    let mut first_bar_beat = 0.0;
    if !bars.is_empty() {
        let thr = -40.0;
        if let Some(i) = (0..bars.len()).find(|&i| bars.start[i] >= -0.03 && bars.level_db[i] > thr)
        {
            first_bar_beat = bars.bar_index(i) as f64 * bpb;
        }
    }
    push(Some(0), CueKind::FirstBeat, first_bar_beat);

    // --- slot 1: mix-in. The phrase line where the drums are established, provided that
    // leaves at least 16 bars before the first drop for the blend; otherwise the first beat
    // (a long beatless intro is mixed in from its start).
    let first_drop_beat = sections
        .iter()
        .find(|s| s.kind == SectionKind::Drop)
        .map(|s| snap_beat(s.start));
    let phrase_floor = |b: f64| pfloor(b, PHRASE_BEATS);
    let start_phrase = pround(first_bar_beat, PHRASE_BEATS);
    let mixin_fallback = if duration > 180.0 {
        ob + PHRASE_BEATS
    } else {
        ob
    };
    let mut mix_in = if bars.is_empty() {
        mixin_fallback
    } else {
        start_phrase.max(ob)
    };
    if !bars.is_empty() {
        let drums_at = (0..bars.len().saturating_sub(1)).find(|&i| {
            bars.start[i] >= -0.03 && bars.stems[i][0] > 0.5 && bars.stems[i + 1][0] > 0.5
        });
        if let Some(i) = drums_at {
            let beat = bars.bar_index(i) as f64 * bpb;
            // Drums entering within 2 bars after a phrase line count from that line.
            let cand = if beat - phrase_floor(beat) <= 2.0 * bpb {
                phrase_floor(beat)
            } else {
                phrase_floor(beat) + PHRASE_BEATS
            };
            let room = first_drop_beat.map(|d| d - cand).unwrap_or(f64::INFINITY);
            if cand > mix_in && room >= 16.0 * bpb && at_beat(cand) < duration * 0.4 {
                mix_in = cand;
            }
        }
    }
    push(Some(1), CueKind::MixIn, mix_in);

    // --- slot 2: vocal entry.
    if bars.has_stems && !bars.is_empty() {
        let n = bars.len();
        let v = |i: usize| bars.stems[i][3];
        if let Some(mut i) = (0..n.saturating_sub(1))
            .find(|&i| bars.start[i] >= -0.03 && v(i) > 0.5 && v(i + 1) > 0.4)
        {
            // Snap to the 4-bar phrase grid when within a bar: a vocal starting in the last bar
            // of a group is a pickup into the next phrase; one starting a bar late (after a
            // vocal chop / reverb tail) belongs to the phrase line before it.
            match (bars.bar_index(i) - bars.phrase_origin).rem_euclid(4) {
                3 if i + 1 < n => i += 1,
                1 if i > 0 && bars.start[i - 1] >= -0.03 => i -= 1,
                _ => {}
            }
            push(Some(2), CueKind::VocalIn, bars.bar_index(i) as f64 * bpb);
        }
    }

    // --- structure-based cues, on runs of equally labelled sections.
    struct Run {
        kind: SectionKind,
        beat: f64,
        bars: u32,
    }
    let mut runs: Vec<Run> = Vec::new();
    for s in sections {
        match runs.last_mut() {
            Some(r) if r.kind == s.kind => r.bars += s.bars,
            _ => runs.push(Run {
                kind: s.kind,
                beat: snap_beat(s.start),
                bars: s.bars,
            }),
        }
    }
    let mut used = vec![false; runs.len()];
    let first_drop = runs.iter().position(|r| r.kind == SectionKind::Drop);
    if let Some(fd) = first_drop {
        if fd > 0 {
            let p = &runs[fd - 1];
            match p.kind {
                SectionKind::Buildup => {
                    push(Some(3), CueKind::Buildup, p.beat);
                    used[fd - 1] = true;
                }
                SectionKind::Breakdown => {
                    push(Some(3), CueKind::Breakdown, p.beat);
                    used[fd - 1] = true;
                }
                _ => {}
            }
        }
        push(Some(4), CueKind::Drop, runs[fd].beat);
        used[fd] = true;
    }
    // Main breakdown: the longest breakdown after the first drop (or anywhere without a drop).
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

    // --- slot 7: mix-out.
    let fallback_out = if duration > 180.0 {
        pfloor(total_beats - PHRASE_BEATS, PHRASE_BEATS)
    } else {
        pfloor(total_beats - 16.0, 16.0)
    };
    let phrase_round = |b: f64| {
        let m32 = pround(b, PHRASE_BEATS);
        if (m32 - b).abs() <= 8.0 {
            m32
        } else {
            pround(b, 16.0)
        }
    };
    let mut mix_out = fallback_out;
    if let Some(o) = outro {
        let ob = runs[o].beat;
        let bars_left = (total_beats - ob) / bpb;
        if (8.0..=64.0).contains(&bars_left) {
            mix_out = phrase_round(ob);
        } else if bars_left > 64.0 {
            mix_out = pfloor(total_beats - 32.0 * bpb, PHRASE_BEATS);
        }
    } else if !runs.is_empty() && duration > 240.0 {
        mix_out = pfloor(total_beats - 32.0 * bpb, PHRASE_BEATS);
    }
    if mix_out <= mix_in + 4.0 * PHRASE_BEATS {
        mix_out = fallback_out.max(mix_in + PHRASE_BEATS);
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
    cues.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    // Remove slot-less duplicates at the position of a slotted cue.
    let slotted: Vec<f64> = cues
        .iter()
        .filter(|c| c.slot.is_some())
        .map(|c| c.pos)
        .collect();
    cues.retain(|c| c.slot.is_some() || !slotted.iter().any(|p| (p - c.pos).abs() < 0.05));
    cues
}
