//! Tracks as the engine sees them: decoded audio plus everything the deck needs from the analysis.

use clubcraft_audio::AudioBuffer;
use clubcraft_core::{effective_cues, Analysis, BeatGrid, Cue, CueKind, Key, StemTrack, Track, SAMPLE_RATE};
use std::sync::Arc;

/// A track ready to be played by a [`crate::Deck`]. Shared with the audio thread through an `Arc`;
/// the engine never mutates it (hot cue edits live in the deck and are reported in
/// [`crate::DeckState::hot_cues`]).
///
/// Build one with [`LoadedTrack::from_parts`] from the library `Track`, its `Analysis`, the
/// (possibly still filling) stems and the decoded audio.
#[derive(Clone)]
pub struct LoadedTrack {
    /// Library id, if the track comes from the library.
    pub track_id: Option<i64>,
    pub title: String,
    pub artist: String,
    /// Decoded planar stereo audio, normally at [`SAMPLE_RATE`]. Shared, never copied.
    pub audio: Arc<AudioBuffer>,
    /// Separated stems; may still be filling progressively. Regions whose chunk is not ready play
    /// the original audio, and the engine crossfades smoothly when chunks appear.
    pub stems: Option<Arc<StemTrack>>,
    pub grid: Option<BeatGrid>,
    pub key: Option<Key>,
    /// All cues (auto + user). Cues with `slot < 8` become hot cues; `MixIn` / `MixOut` drive
    /// automix; the first `FirstBeat` cue becomes the main cue point.
    pub cues: Vec<Cue>,
    /// Loudness normalisation gain from the analysis (applied as auto-gain when enabled).
    pub gain_db: Option<f32>,
}

impl std::fmt::Debug for LoadedTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedTrack")
            .field("track_id", &self.track_id)
            .field("title", &self.title)
            .field("frames", &self.audio.frames())
            .field("stems", &self.stems.is_some())
            .field("grid", &self.grid)
            .field("key", &self.key)
            .field("cues", &self.cues.len())
            .field("gain_db", &self.gain_db)
            .finish()
    }
}

impl LoadedTrack {
    /// A bare track without analysis.
    pub fn new(title: impl Into<String>, audio: Arc<AudioBuffer>) -> Self {
        LoadedTrack {
            track_id: None,
            title: title.into(),
            artist: String::new(),
            audio,
            stems: None,
            grid: None,
            key: None,
            cues: Vec::new(),
            gain_db: None,
        }
    }

    /// From the library: track metadata, analysis (grid, key, gain, auto cues), stems (may be
    /// partial or `None`) and the decoded audio. Add user cues with [`Self::with_user_cues`].
    pub fn from_parts(track: &Track, analysis: &Analysis, stems: Option<Arc<StemTrack>>, audio: Arc<AudioBuffer>) -> Self {
        let mut t = LoadedTrack::new(track.title.clone(), audio);
        t.track_id = Some(track.id);
        t.artist = track.artist.clone();
        t.stems = stems;
        t.grid = analysis.grid;
        t.key = analysis.key;
        t.gain_db = analysis.gain_db;
        t.cues = analysis.cues.clone();
        t
    }

    /// Merge user cues over the analysis cues (a user cue on a pad slot replaces the auto cue).
    pub fn with_user_cues(mut self, user: &[Cue]) -> Self {
        let auto: Vec<Cue> = self.cues.iter().filter(|c| c.source == clubcraft_core::CueSource::Auto).cloned().collect();
        let mut users: Vec<Cue> = self.cues.iter().filter(|c| c.source != clubcraft_core::CueSource::Auto).cloned().collect();
        users.extend_from_slice(user);
        self.cues = effective_cues(&auto, &users);
        self
    }

    pub fn with_stems(mut self, stems: Arc<StemTrack>) -> Self {
        self.stems = Some(stems);
        self
    }

    pub fn with_grid(mut self, grid: BeatGrid) -> Self {
        self.grid = Some(grid);
        self
    }

    pub fn with_cues(mut self, cues: Vec<Cue>) -> Self {
        self.cues = cues;
        self
    }

    pub fn frames(&self) -> usize {
        self.audio.frames()
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.sample_rate() as f64
    }

    pub fn sample_rate(&self) -> u32 {
        if self.audio.sample_rate == 0 {
            SAMPLE_RATE
        } else {
            self.audio.sample_rate
        }
    }

    pub fn bpm(&self) -> Option<f64> {
        self.grid.map(|g| g.bpm)
    }

    pub fn cue(&self, kind: CueKind) -> Option<&Cue> {
        self.cues.iter().find(|c| c.kind == kind)
    }

    /// Analysis-like view (duration + grid + key + gain + cues) for building a
    /// [`clubcraft_core::MixTimeline`] from loaded tracks.
    pub fn analysis(&self) -> Analysis {
        Analysis {
            version: Analysis::CURRENT_VERSION,
            duration: self.duration(),
            grid: self.grid,
            key: self.key,
            gain_db: self.gain_db,
            cues: self.cues.clone(),
            ..Default::default()
        }
    }

    /// Where a newly loaded deck parks: the first-beat cue, else the grid's first downbeat, else 0.
    pub fn start_position(&self) -> f64 {
        if let Some(c) = self.cue(CueKind::FirstBeat) {
            return c.pos.max(0.0);
        }
        if let Some(g) = self.grid {
            let first = g.time_of_bar(g.bar_at(0.0).ceil());
            return first.max(0.0);
        }
        0.0
    }

    /// Mix-in point: the `MixIn` cue, else the start position.
    pub fn mix_in(&self) -> f64 {
        self.cue(CueKind::MixIn).map(|c| c.pos).unwrap_or_else(|| self.start_position())
    }

    /// Mix-out point: the `MixOut` cue, else the last bar line that leaves `beats` beats before
    /// the end of the track.
    pub fn mix_out(&self, beats: u32) -> f64 {
        if let Some(c) = self.cue(CueKind::MixOut) {
            return c.pos;
        }
        let dur = self.duration();
        match self.grid {
            Some(g) => {
                let t = dur - beats as f64 * g.beat_len();
                g.time_of_bar(g.bar_at(t).floor()).max(self.mix_in())
            }
            None => (dur - beats as f64 * 0.5).max(0.0),
        }
    }
}

/// Test / tooling helper: a stereo buffer from a mono signal.
pub fn buffer_from_mono(sample_rate: u32, mono: &[f32]) -> AudioBuffer {
    AudioBuffer::new(sample_rate, mono.to_vec(), mono.to_vec())
}
