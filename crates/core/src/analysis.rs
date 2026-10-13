use crate::key::Key;
use serde::{Deserialize, Serialize};

/// The four stems produced by the separator, in the model's output order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StemKind {
    Drums = 0,
    Bass = 1,
    /// "Melody" in the UI (Demucs calls it `other`): synths, keys, guitars, fx.
    Other = 2,
    Vocals = 3,
}

impl StemKind {
    pub const ALL: [StemKind; 4] = [StemKind::Drums, StemKind::Bass, StemKind::Other, StemKind::Vocals];
    pub const COUNT: usize = 4;

    pub fn label(self) -> &'static str {
        match self {
            StemKind::Drums => "Drums",
            StemKind::Bass => "Bass",
            StemKind::Other => "Melody",
            StemKind::Vocals => "Vocals",
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// Display colour (RGB), shared by waveforms, pads and the studio timeline.
    pub fn color(self) -> [u8; 3] {
        match self {
            StemKind::Drums => [255, 176, 32],
            StemKind::Bass => [235, 64, 80],
            StemKind::Other => [64, 200, 255],
            StemKind::Vocals => [120, 230, 110],
        }
    }
}

/// A constant-tempo beat grid. Electronic dance music is almost always constant tempo, so the
/// grid is a tempo plus the time of one downbeat (beat 1 of a bar). Beat indices are counted from
/// that anchor; negative indices are valid (beats before the anchor).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeatGrid {
    pub bpm: f64,
    /// Time (s) of the first downbeat at or after the start of the track.
    pub downbeat: f64,
    pub beats_per_bar: u32,
}

impl BeatGrid {
    pub fn new(bpm: f64, downbeat: f64) -> Self {
        BeatGrid { bpm, downbeat, beats_per_bar: 4 }
    }

    pub fn beat_len(&self) -> f64 {
        60.0 / self.bpm
    }

    pub fn bar_len(&self) -> f64 {
        self.beat_len() * self.beats_per_bar as f64
    }

    /// Fractional beat index at time `t` (0.0 = the anchor downbeat).
    pub fn beat_at(&self, t: f64) -> f64 {
        (t - self.downbeat) / self.beat_len()
    }

    pub fn time_of_beat(&self, beat: f64) -> f64 {
        self.downbeat + beat * self.beat_len()
    }

    /// Fractional bar index at time `t`.
    pub fn bar_at(&self, t: f64) -> f64 {
        self.beat_at(t) / self.beats_per_bar as f64
    }

    pub fn time_of_bar(&self, bar: f64) -> f64 {
        self.time_of_beat(bar * self.beats_per_bar as f64)
    }

    /// Snap `t` to the nearest multiple of `beats` (e.g. 1.0 = beat, 0.25 = 1/16 note, 4 = bar).
    pub fn snap(&self, t: f64, beats: f64) -> f64 {
        let b = self.beat_at(t) / beats;
        self.time_of_beat(b.round() * beats)
    }

    /// Phase within the beat in 0..1.
    pub fn beat_phase(&self, t: f64) -> f64 {
        self.beat_at(t).rem_euclid(1.0)
    }

    /// Position within the bar, 0..beats_per_bar.
    pub fn beat_in_bar(&self, t: f64) -> f64 {
        self.beat_at(t).rem_euclid(self.beats_per_bar as f64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SectionKind {
    Intro,
    Verse,
    Buildup,
    Drop,
    Breakdown,
    Outro,
}

impl SectionKind {
    pub fn label(self) -> &'static str {
        match self {
            SectionKind::Intro => "Intro",
            SectionKind::Verse => "Verse",
            SectionKind::Buildup => "Build",
            SectionKind::Drop => "Drop",
            SectionKind::Breakdown => "Break",
            SectionKind::Outro => "Outro",
        }
    }

    pub fn color(self) -> [u8; 3] {
        match self {
            SectionKind::Intro => [90, 140, 255],
            SectionKind::Verse => [150, 120, 255],
            SectionKind::Buildup => [255, 210, 60],
            SectionKind::Drop => [255, 80, 80],
            SectionKind::Breakdown => [60, 200, 170],
            SectionKind::Outro => [90, 140, 255],
        }
    }
}

/// A structural section (phrase group) of a track, aligned to the beat grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub kind: SectionKind,
    pub start: f64,
    pub end: f64,
    /// Bar index of `start` relative to the grid anchor.
    pub start_bar: i32,
    pub bars: u32,
    /// Relative energy 0..1 within the track.
    pub energy: f32,
    /// Mean stem activity (drums, bass, other, vocals) in 0..1.
    pub stems: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CueKind {
    /// Generic hot cue.
    Hot,
    /// First downbeat: where the track should be started from.
    FirstBeat,
    /// Recommended point to start mixing this track in.
    MixIn,
    /// Recommended point to start mixing out of this track.
    MixOut,
    VocalIn,
    Drop,
    Breakdown,
    Buildup,
    Outro,
    /// Saved loop (`loop_beats` is set).
    Loop,
}

impl CueKind {
    pub fn label(self) -> &'static str {
        match self {
            CueKind::Hot => "Cue",
            CueKind::FirstBeat => "Start",
            CueKind::MixIn => "Mix In",
            CueKind::MixOut => "Mix Out",
            CueKind::VocalIn => "Vocal",
            CueKind::Drop => "Drop",
            CueKind::Breakdown => "Break",
            CueKind::Buildup => "Build",
            CueKind::Outro => "Outro",
            CueKind::Loop => "Loop",
        }
    }

    pub fn default_color(self) -> [u8; 3] {
        match self {
            CueKind::Hot => [230, 230, 230],
            CueKind::FirstBeat => [80, 220, 120],
            CueKind::MixIn => [60, 150, 255],
            CueKind::MixOut => [255, 120, 40],
            CueKind::VocalIn => [120, 230, 110],
            CueKind::Drop => [255, 60, 70],
            CueKind::Breakdown => [60, 200, 170],
            CueKind::Buildup => [255, 210, 60],
            CueKind::Outro => [170, 110, 255],
            CueKind::Loop => [255, 170, 0],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CueSource {
    /// Generated by ClubCraft's structure analysis.
    Auto,
    /// Set or edited by the user (never overwritten by re-analysis).
    User,
    /// Imported from another application (DJ.Studio, VirtualDJ, rekordbox ...).
    Imported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    /// Hot cue pad slot (0-based). `None` = memory cue / marker only.
    pub slot: Option<u8>,
    pub kind: CueKind,
    pub pos: f64,
    pub name: String,
    pub color: [u8; 3],
    /// For saved loops: loop length in beats.
    pub loop_beats: Option<f64>,
    pub source: CueSource,
}

impl Cue {
    pub fn auto(slot: Option<u8>, kind: CueKind, pos: f64) -> Self {
        Cue {
            slot,
            kind,
            pos,
            name: kind.label().to_string(),
            color: kind.default_color(),
            loop_beats: None,
            source: CueSource::Auto,
        }
    }
}

/// Number of hot cue pads per deck.
pub const HOT_CUES: usize = 8;

/// Compact multi-band waveform summary used by every waveform view.
///
/// `rate` bins per second; each bin stores peak amplitudes 0..255. `bands` are low / mid / high
/// of the full mix; `stems` (if separated) are the per-stem peaks in [`StemKind`] order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Waveform {
    pub rate: f64,
    pub peak: Vec<u8>,
    pub low: Vec<u8>,
    pub mid: Vec<u8>,
    pub high: Vec<u8>,
    pub stems: Option<[Vec<u8>; 4]>,
}

impl Waveform {
    pub fn len(&self) -> usize {
        self.peak.len()
    }
    pub fn is_empty(&self) -> bool {
        self.peak.is_empty()
    }
    pub fn duration(&self) -> f64 {
        self.peak.len() as f64 / self.rate.max(1e-9)
    }
}

/// Everything ClubCraft knows about a track's audio. Stored as JSON in the library.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    /// Bumped when the analysis algorithms change, so stale results are recomputed.
    pub version: u32,
    pub duration: f64,
    pub grid: Option<BeatGrid>,
    pub key: Option<Key>,
    /// Integrated loudness (LUFS) and the gain (dB) that brings the track to the target level.
    pub loudness_lufs: Option<f32>,
    pub gain_db: Option<f32>,
    /// Energy rating 1..=10.
    pub energy: Option<u8>,
    pub sections: Vec<Section>,
    /// Automatically generated cues (user cues are stored separately in the library).
    pub cues: Vec<Cue>,
    /// Model used for the cached stems, if any.
    pub stems_model: Option<String>,
}

impl Analysis {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn bpm(&self) -> Option<f64> {
        self.grid.map(|g| g.bpm)
    }

    pub fn cue(&self, kind: CueKind) -> Option<&Cue> {
        self.cues.iter().find(|c| c.kind == kind)
    }

    pub fn section_at(&self, t: f64) -> Option<&Section> {
        self.sections.iter().find(|s| t >= s.start && t < s.end)
    }
}

/// Merge auto cues with user cues: a user cue occupying a pad slot replaces the auto cue there.
pub fn effective_cues(auto: &[Cue], user: &[Cue]) -> Vec<Cue> {
    let mut out: Vec<Cue> = user.to_vec();
    for c in auto {
        let taken = c.slot.is_some() && user.iter().any(|u| u.slot == c.slot);
        if !taken {
            out.push(c.clone());
        }
    }
    out.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_math() {
        let g = BeatGrid::new(120.0, 1.0);
        assert_eq!(g.beat_len(), 0.5);
        assert_eq!(g.beat_at(3.0), 4.0);
        assert_eq!(g.bar_at(3.0), 1.0);
        assert_eq!(g.time_of_bar(2.0), 5.0);
        assert!((g.snap(3.1, 1.0) - 3.0).abs() < 1e-9);
        assert!((g.beat_in_bar(0.5) - 3.0).abs() < 1e-9);
    }
}
