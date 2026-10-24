//! Functional section labels per bar (chorus, verse, bridge ...) from the All-In-One structure
//! model ([`crate::allinone`]), for choosing vocal phrases in the mashup finder.
//!
//! ClubCraft's own sections describe the arrangement of dance music (intro, build, drop,
//! breakdown); they say where the drums and bass are, not what the singer does. The model's
//! labels come from the Harmonix pop annotations and say exactly that: where the verse is and
//! where the chorus. Measured on vocal house in the library, they mark verse and chorus
//! cleanly (Come Find Me: verse bars 39-60, chorus 69-84 and 119-148).

use crate::allinone::{self, Activations, LABELS};
use clubcraft_core::BeatGrid;

/// Bumped when the stored labels change.
pub const VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"CCFL";
/// Runs of a label shorter than this many bars are taken as model jitter.
const MIN_RUN: usize = 4;

/// The model's labels without its `start` / `end` markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Label {
    Intro,
    Outro,
    Break,
    Bridge,
    Inst,
    Solo,
    Verse,
    Chorus,
}

impl Label {
    const ALL: [Label; 8] = [Label::Intro, Label::Outro, Label::Break, Label::Bridge, Label::Inst, Label::Solo, Label::Verse, Label::Chorus];

    /// Index in the model's output ([`LABELS`]).
    pub fn index(self) -> usize {
        self as usize + 2
    }

    pub fn name(self) -> &'static str {
        LABELS[self.index()]
    }
}

/// A run of bars with one label.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelRun {
    pub label: Label,
    pub start_bar: i32,
    pub bars: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BarLabels {
    pub version: u32,
    /// Grid the bars refer to.
    pub bpm: f64,
    pub downbeat: f64,
    pub beats_per_bar: u32,
    /// `probs[j]` belongs to bar `first_bar + j`.
    pub first_bar: i32,
    /// Mean label probabilities per bar, in the model's order.
    pub probs: Vec<[f32; 10]>,
}

impl BarLabels {
    /// Average the model's frames per bar of `grid`.
    pub fn from_activations(act: &Activations, grid: BeatGrid, duration: f64) -> BarLabels {
        let first_bar = grid.bar_at(0.0).ceil() as i32;
        let last_bar = grid.bar_at(duration.min(act.len() as f64 / allinone::FPS)).floor() as i32;
        let probs = (first_bar..last_bar).map(|b| allinone::mean_labels(act, grid.time_of_bar(b as f64), grid.time_of_bar(b as f64 + 1.0))).collect();
        BarLabels { version: VERSION, bpm: grid.bpm, downbeat: grid.downbeat, beats_per_bar: grid.beats_per_bar, first_bar, probs }
    }

    pub fn grid(&self) -> BeatGrid {
        BeatGrid { bpm: self.bpm, downbeat: self.downbeat, beats_per_bar: self.beats_per_bar }
    }

    /// Probability of `label` in `bar` (0 outside the track).
    pub fn p(&self, bar: i32, label: Label) -> f32 {
        usize::try_from(bar - self.first_bar).ok().and_then(|j| self.probs.get(j)).map(|p| p[label.index()]).unwrap_or(0.0)
    }

    /// Mean probability of `label` over `bars` bars from `start`.
    pub fn mean(&self, start: i32, bars: usize, label: Label) -> f32 {
        (0..bars as i32).map(|k| self.p(start + k, label)).sum::<f32>() / bars.max(1) as f32
    }

    /// The most likely label of `bar`.
    pub fn label(&self, bar: i32) -> Option<Label> {
        let p = usize::try_from(bar - self.first_bar).ok().and_then(|j| self.probs.get(j))?;
        Label::ALL.into_iter().max_by(|a, b| p[a.index()].total_cmp(&p[b.index()]))
    }

    /// The track as runs of one label; runs shorter than four bars join the run before them.
    pub fn runs(&self) -> Vec<LabelRun> {
        let mut runs: Vec<LabelRun> = Vec::new();
        for j in 0..self.probs.len() {
            let bar = self.first_bar + j as i32;
            let Some(l) = self.label(bar) else { continue };
            match runs.last_mut() {
                Some(r) if r.label == l => r.bars += 1,
                _ => runs.push(LabelRun { label: l, start_bar: bar, bars: 1 }),
            }
        }
        let mut out: Vec<LabelRun> = Vec::new();
        for r in runs {
            match out.last_mut() {
                Some(prev) if (r.bars as usize) < MIN_RUN || prev.label == r.label => prev.bars += r.bars,
                _ => out.push(r),
            }
        }
        // a short run at the very start has nothing before it to join
        if out.len() > 1 && (out[0].bars as usize) < MIN_RUN {
            let first = out.remove(0);
            out[0].start_bar = first.start_bar;
            out[0].bars += first.bars;
        }
        out
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(40 + self.probs.len() * 40);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&self.version.to_le_bytes());
        b.extend_from_slice(&self.bpm.to_le_bytes());
        b.extend_from_slice(&self.downbeat.to_le_bytes());
        b.extend_from_slice(&self.beats_per_bar.to_le_bytes());
        b.extend_from_slice(&self.first_bar.to_le_bytes());
        b.extend_from_slice(&(self.probs.len() as u32).to_le_bytes());
        for p in &self.probs {
            for v in p {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<BarLabels> {
        let mut p = 0usize;
        let mut take = |n: usize| -> Option<&[u8]> {
            let s = b.get(p..p + n)?;
            p += n;
            Some(s)
        };
        if take(4)? != MAGIC {
            return None;
        }
        let version = u32::from_le_bytes(take(4)?.try_into().ok()?);
        let bpm = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let downbeat = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let beats_per_bar = u32::from_le_bytes(take(4)?.try_into().ok()?);
        let first_bar = i32::from_le_bytes(take(4)?.try_into().ok()?);
        let n = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let mut probs = Vec::with_capacity(n.min(100_000));
        for _ in 0..n {
            let raw = take(40)?;
            let mut row = [0f32; 10];
            for (v, c) in row.iter_mut().zip(raw.as_chunks::<4>().0) {
                *v = f32::from_le_bytes(*c);
            }
            probs.push(row);
        }
        Some(BarLabels { version, bpm, downbeat, beats_per_bar, first_bar, probs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(seq: &[(Label, usize)]) -> BarLabels {
        let mut probs = Vec::new();
        for &(l, n) in seq {
            for _ in 0..n {
                let mut p = [0.02f32; 10];
                p[l.index()] = 0.8;
                probs.push(p);
            }
        }
        BarLabels { version: VERSION, bpm: 124.0, downbeat: 0.0, beats_per_bar: 4, first_bar: 0, probs }
    }

    #[test]
    fn short_runs_join_their_neighbours() {
        let l = labels(&[(Label::Intro, 16), (Label::Verse, 2), (Label::Intro, 2), (Label::Verse, 14), (Label::Chorus, 16), (Label::Inst, 1), (Label::Chorus, 15)]);
        let r = l.runs();
        let kinds: Vec<(Label, i32, u32)> = r.iter().map(|r| (r.label, r.start_bar, r.bars)).collect();
        assert_eq!(kinds, vec![(Label::Intro, 0, 20), (Label::Verse, 20, 14), (Label::Chorus, 34, 32)]);
        assert!((l.mean(34, 16, Label::Chorus) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn bytes_roundtrip() {
        let l = labels(&[(Label::Verse, 3), (Label::Chorus, 2)]);
        assert_eq!(BarLabels::from_bytes(&l.to_bytes()), Some(l));
    }
}
