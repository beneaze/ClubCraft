//! Audio → UI state snapshot ([`EngineState`]), published lock-free by the audio thread after
//! every processed device buffer. All types are plain `Copy` data (no heap), so publishing never
//! allocates. Poll it with [`crate::EngineHandle::state`] (cheap; intended for 60 fps).

use crate::fx::FxKind;
use crate::mixer::{CrossfaderAssign, CrossfaderCurve};
use clubcraft_core::{CueKind, Key};
use serde::{Deserialize, Serialize};

/// Maximum number of decks the engine can run.
pub const MAX_DECKS: usize = 4;
/// Hot cue pads per deck.
pub const HOT_CUES: usize = clubcraft_core::HOT_CUES;
/// Effect slots per deck.
pub const FX_SLOTS: usize = 3;
/// `MixPlaybackState::resident` covers the first `64 * MIX_RESIDENT_WORDS` mix tracks.
pub const MIX_RESIDENT_WORDS: usize = 4;

/// Cue button behaviour (VirtualDJ `cueMode`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CueMode {
    /// CDJ: stopped → set the cue (or preview while held when already on it); playing → return
    /// to the cue and stop.
    #[default]
    Cdj,
    /// Plays from the cue while held, returns to the cue on release.
    Hold,
    /// Cue-Up-Play: press parks on the cue, release plays.
    Cup,
}

/// What a hot cue does when the deck is stopped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HotCueMode {
    /// Plays while held, returns to the hot cue on release (CDJ style).
    #[default]
    Preview,
    /// Starts playback from the hot cue.
    Play,
}

/// Pitch slider range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PitchRange {
    Pct6,
    #[default]
    Pct8,
    Pct10,
    Pct16,
    Pct25,
    Pct50,
    Pct100,
}

impl PitchRange {
    pub const ALL: [PitchRange; 7] =
        [PitchRange::Pct6, PitchRange::Pct8, PitchRange::Pct10, PitchRange::Pct16, PitchRange::Pct25, PitchRange::Pct50, PitchRange::Pct100];

    pub fn percent(self) -> u32 {
        match self {
            PitchRange::Pct6 => 6,
            PitchRange::Pct8 => 8,
            PitchRange::Pct10 => 10,
            PitchRange::Pct16 => 16,
            PitchRange::Pct25 => 25,
            PitchRange::Pct50 => 50,
            PitchRange::Pct100 => 100,
        }
    }

    pub fn fraction(self) -> f64 {
        self.percent() as f64 / 100.0
    }

    pub fn from_percent(p: u32) -> Option<PitchRange> {
        Self::ALL.into_iter().find(|r| r.percent() == p)
    }

    /// Next wider range (wraps to the smallest).
    pub fn next(self) -> PitchRange {
        let i = Self::ALL.iter().position(|r| *r == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

/// A hot cue pad.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HotCue {
    /// Position (s) in the track.
    pub pos: f64,
    pub kind: CueKind,
    pub color: [u8; 3],
    /// Saved loop length in beats (pressing the pad engages the loop).
    pub loop_beats: Option<f64>,
}

/// Loop state (positions in seconds of the track).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoopState {
    /// A loop region exists (may be inactive: `reloop` re-engages it).
    pub defined: bool,
    pub active: bool,
    pub start: f64,
    pub end: f64,
    /// Length in beats when the deck has a grid.
    pub beats: Option<f64>,
    /// Momentary loop roll (slip) in progress.
    pub roll: bool,
    /// Pending loop-in point set with `loop_in`, waiting for `loop_out`.
    pub loop_in_mark: Option<f64>,
    /// Size used by the auto-loop button / `loop_halve` / `loop_double` when no loop is set.
    pub auto_beats: f64,
}

/// Peak and RMS levels (linear amplitude, 1.0 = 0 dBFS).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    pub peak: [f32; 2],
    pub rms: [f32; 2],
}

impl Levels {
    pub fn peak_db(&self) -> f32 {
        crate::dsp::gain_to_db(self.peak[0].max(self.peak[1]))
    }
}

/// State of one deck FX slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FxSlotState {
    pub kind: FxKind,
    pub active: bool,
    pub wet: f32,
    pub beats: f32,
    pub params: [f32; 2],
}

/// Everything the UI needs to draw one deck + its mixer channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeckState {
    pub loaded: bool,
    pub track_id: Option<i64>,
    /// Incremented on every load, so the UI can tell two loads of the same track apart.
    pub load_serial: u64,
    /// Seconds.
    pub duration: f64,
    /// Heard playhead position (s, in the original track timeline; latency compensated).
    pub position: f64,
    pub playing: bool,
    /// Playing only while a cue / hot cue button is held.
    pub previewing: bool,
    pub reverse: bool,
    pub scratching: bool,
    /// A brake / spinback is in progress.
    pub braking: bool,
    pub slip_mode: bool,
    /// Ghost position while slipping (where playback resumes).
    pub slip_position: Option<f64>,
    /// Track BPM from the grid.
    pub bpm: Option<f64>,
    /// BPM as heard (grid bpm × tempo).
    pub effective_bpm: Option<f64>,
    /// Tempo ratio (1.0 = original speed), including sync and pitch bend.
    pub tempo: f64,
    /// Pitch as a percentage (tempo − 1) × 100.
    pub pitch_percent: f64,
    /// Pitch slider position −1..1.
    pub pitch_slider: f32,
    pub pitch_range: PitchRange,
    pub keylock: bool,
    /// Semitones.
    pub key_shift: f32,
    /// Track key from the analysis.
    pub key: Option<Key>,
    /// Key as heard (key shift + varispeed when keylock is off), rounded to a semitone.
    pub effective_key: Option<Key>,
    /// The time stretcher is running for this deck.
    pub stretching: bool,
    /// Main cue point (s).
    pub cue: f64,
    pub cue_mode: CueMode,
    pub hotcue_mode: HotCueMode,
    pub hot_cues: [Option<HotCue>; HOT_CUES],
    pub loop_state: LoopState,
    pub quantize: bool,
    pub sync: bool,
    pub is_master: bool,
    /// Phase difference to the master deck in beats (−0.5..0.5), when both have grids.
    pub phase_offset: Option<f64>,
    /// Beat index (fractional) at the playhead, when the deck has a grid.
    pub beat: Option<f64>,
    /// Effective stem gains (user × automation) in StemKind order.
    pub stem_gains: [f32; 4],
    pub stem_muted: [bool; 4],
    /// The track has (possibly partial) stems.
    pub has_stems: bool,
    /// Fraction of stem chunks separated so far.
    pub stems_ready: f32,
    /// Stem echo send per stem (0..1).
    pub stem_echo: [f32; 4],
    // ---- mixer channel ----
    /// User trim (dB) and auto gain (dB) currently applied.
    pub trim_db: f32,
    pub auto_gain_db: f32,
    /// EQ knobs low, mid, high (0..1, 0.5 = flat).
    pub eq: [f32; 3],
    pub filter: f32,
    pub filter_resonance: f32,
    pub fader: f32,
    pub crossfader_assign: CrossfaderAssign,
    pub pfl: bool,
    /// Post-fader channel level.
    pub level: Levels,
    pub fx: [FxSlotState; FX_SLOTS],
    pub echo_send: f32,
}

/// Master section state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MasterState {
    pub gain_db: f32,
    pub level: Levels,
    /// Lowest limiter gain in the last buffer (1.0 = not limiting).
    pub limiter_gain: f32,
    pub limiter_enabled: bool,
    pub crossfader: f32,
    pub crossfader_curve: CrossfaderCurve,
    pub headphone_mix: f32,
    pub headphone_gain_db: f32,
    pub headphone_level: Levels,
}

/// Mix playback (studio preview / automix) state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MixPlaybackState {
    /// A mix is loaded into the conductor.
    pub active: bool,
    pub playing: bool,
    /// Mix time (s).
    pub time: f64,
    pub duration: f64,
    /// Index (in the mix) of the track that is currently "on air" (the incoming one once a
    /// transition passes its midpoint).
    pub current_track: usize,
    /// `Some((transition index, beat within the transition))` during a transition.
    pub transition: Option<(usize, f32)>,
    /// Mix tempo (BPM) right now.
    pub bpm: f64,
    pub finished: bool,
    /// Distinguishes successive mixes.
    pub serial: u64,
    /// [`crate::MixSchedule::id`] of the schedule being played (0 = none).
    pub schedule_id: u64,
    /// Bit set of mix tracks whose audio is resident on the audio thread
    /// (bit `i % 64` of word `i / 64`).
    pub resident: [u64; MIX_RESIDENT_WORDS],
    /// A track that should be audible right now has no audio yet (playing silence).
    pub waiting: bool,
}

impl MixPlaybackState {
    /// Whether mix track `i` is resident (always `false` beyond the bit set).
    pub fn is_resident(&self, i: usize) -> bool {
        i < 64 * MIX_RESIDENT_WORDS && self.resident[i / 64] & (1 << (i % 64)) != 0
    }

    pub fn resident_count(&self) -> u32 {
        self.resident.iter().map(|w| w.count_ones()).sum()
    }
}

/// Recording state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordingState {
    pub active: bool,
    pub seconds: f64,
    /// Samples dropped because the writer thread fell behind (should stay 0).
    pub dropped: u64,
}

/// Complete engine snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineState {
    /// Output sample rate.
    pub sample_rate: u32,
    pub deck_count: usize,
    pub decks: [DeckState; MAX_DECKS],
    pub master: MasterState,
    /// Deck that is the current sync master.
    pub master_deck: Option<usize>,
    /// `true` when the master deck is chosen automatically.
    pub master_auto: bool,
    pub quantize_beats: f64,
    pub auto_gain: bool,
    pub mix: MixPlaybackState,
    pub recording: RecordingState,
    /// Audio callback time / buffer duration (0..1+), smoothed.
    pub cpu_load: f32,
    /// Frames rendered since the engine started.
    pub frames: u64,
    /// The audio callback was late / the device reported an error since start.
    pub stream_errors: u32,
}

impl EngineState {
    pub fn deck(&self, i: usize) -> &DeckState {
        &self.decks[i.min(MAX_DECKS - 1)]
    }
}
