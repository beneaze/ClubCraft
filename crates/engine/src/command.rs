//! UI → audio thread commands. Sent through a lock-free SPSC ring ([`crate::EngineHandle::send`])
//! or applied directly to an offline [`crate::EngineCore`]. Every variant is cheap to move; the
//! few that carry heap data (`LoadTrack`, `PlayMix`, `SetRecorder`) were allocated on the sending
//! thread and are handed back to a collector thread for dropping.

use crate::conductor::MixSchedule;
use crate::fx::FxKind;
use crate::mixer::{CrossfaderAssign, CrossfaderCurve, FaderCurve};
use crate::state::{CueMode, HotCueMode, PitchRange};
use crate::track::LoadedTrack;
use clubcraft_core::StemKind;
use std::sync::Arc;

/// Deck index 0..4 (0 = A / 1, 1 = B / 2, ...).
pub type DeckId = usize;

/// EQ band.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EqBand {
    Low = 0,
    Mid = 1,
    High = 2,
}

/// Every action the engine understands.
#[derive(Debug)]
pub enum Command {
    // ---------------------------------------------------------------- tracks
    /// Load a track (deck stops and parks on the track's start cue).
    LoadTrack(DeckId, Arc<LoadedTrack>),
    Unload(DeckId),

    // ---------------------------------------------------------------- transport
    Play(DeckId),
    Pause(DeckId),
    PlayPause(DeckId),
    /// Pause and return to the cue point.
    Stop(DeckId),
    /// When playing, restart from the cue (VirtualDJ play-stutter); otherwise play.
    PlayStutter(DeckId),
    /// Cue button down / up (behaviour per [`CueMode`]).
    CueDown(DeckId),
    CueUp(DeckId),
    /// Set the cue point at the (quantized) playhead.
    SetCue(DeckId),
    GotoCue(DeckId),
    /// Seek to seconds.
    Seek(DeckId, f64),
    SetCueMode(DeckId, CueMode),
    SetHotCueMode(DeckId, HotCueMode),

    // ---------------------------------------------------------------- hot cues
    /// Pad pressed: empty → set; playing → (quantized) jump; stopped → preview while held.
    HotCueDown(DeckId, usize),
    HotCueUp(DeckId, usize),
    /// Set hot cue `slot` at `pos` seconds (`None` = at the quantized playhead).
    SetHotCue { deck: DeckId, slot: usize, pos: Option<f64> },
    DeleteHotCue(DeckId, usize),

    // ---------------------------------------------------------------- tempo / key
    /// Pitch slider −1..1 (scaled by the range).
    SetPitch(DeckId, f32),
    /// Set the tempo ratio directly (e.g. 1.02 = +2 %); moves the slider.
    SetTempo(DeckId, f64),
    SetPitchRange(DeckId, PitchRange),
    PitchReset(DeckId),
    /// Temporary pitch bend −1..1 (±4 %); send 0 on release.
    PitchBend(DeckId, f32),
    /// Keylock / master tempo.
    SetKeylock(DeckId, bool),
    /// Key shift in semitones (fractional allowed), −12..12.
    SetKeyShift(DeckId, f32),

    // ---------------------------------------------------------------- jog / motion
    /// Platter touch (vinyl mode: scratch while touched).
    JogTouch(DeckId, bool),
    /// Platter movement in revolutions (scratch when touched, nudge when playing).
    Jog(DeckId, f64),
    SetVinylMode(DeckId, bool),
    SetReverse(DeckId, bool),
    /// Brake to a stop over `beats` beats.
    Brake(DeckId, f64),
    Spinback(DeckId),
    SetSlip(DeckId, bool),

    // ---------------------------------------------------------------- loops / jumps
    LoopIn(DeckId),
    LoopOut(DeckId),
    /// Auto loop of N beats (1/32 .. 32); toggles off when the same size is active.
    AutoLoop(DeckId, f64),
    LoopExit(DeckId),
    Reloop(DeckId),
    LoopHalve(DeckId),
    LoopDouble(DeckId),
    /// Move the loop by ±beats.
    LoopMove(DeckId, f64),
    /// Momentary loop roll (slip) of N beats while `on`.
    LoopRoll { deck: DeckId, beats: f64, on: bool },
    /// Jump ±beats (moves an active loop along).
    BeatJump(DeckId, f64),
    SetQuantize(DeckId, bool),
    /// Global quantize resolution in beats (1 = beat, 0.25 = 1/16 ...).
    SetQuantizeBeats(f64),

    // ---------------------------------------------------------------- sync
    /// Match tempo + phase to the master once and stay phase-locked (sets sync on).
    Sync(DeckId),
    /// Turn sync on/off.
    SetSync(DeckId, bool),
    /// `Some(deck)` = manual master, `None` = automatic master selection.
    SetMaster(Option<DeckId>),

    // ---------------------------------------------------------------- stems
    /// Continuous stem volume 0..1 (residual formula: unity = original).
    SetStemGain { deck: DeckId, stem: StemKind, gain: f32 },
    SetStemMute { deck: DeckId, stem: StemKind, mute: bool },
    ToggleStemMute(DeckId, StemKind),
    /// Solo a stem (toggle).
    StemSolo(DeckId, StemKind),
    /// Vocals only (toggle).
    Acapella(DeckId),
    /// Everything but vocals (toggle).
    Instrumental(DeckId),
    StemsReset(DeckId),
    /// Stem echo send: `send` 0..1, delay in beats, feedback 0..0.95.
    StemEcho { deck: DeckId, stem: StemKind, send: f32, beats: f32, feedback: f32 },
    /// Mute the stem and let its last beat echo out.
    StemEchoOut(DeckId, StemKind),

    // ---------------------------------------------------------------- mixer
    /// User trim in dB (on top of auto-gain).
    SetTrim(DeckId, f32),
    /// Apply the analysis loudness gain to every deck.
    SetAutoGain(bool),
    /// EQ knob 0..1 (0 = kill, 0.5 = flat, 1 = +6 dB).
    SetEq(DeckId, EqBand, f32),
    /// Filter knob 0..1 (0.5 off, < LPF, > HPF).
    SetFilter(DeckId, f32),
    SetFilterResonance(DeckId, f32),
    /// Channel fader 0..1.
    SetFader(DeckId, f32),
    SetFaderCurve(FaderCurve),
    /// 0 = A, 1 = B.
    SetCrossfader(f32),
    SetCrossfaderCurve(CrossfaderCurve),
    SetCrossfaderAssign(DeckId, CrossfaderAssign),
    SetPfl(DeckId, bool),
    /// Post-fader echo send 0..1 (the automation echo).
    SetEchoSend(DeckId, f32),
    SetMasterGain(f32),
    SetLimiter { enabled: bool, ceiling_db: f32 },
    /// 0 = cue only, 1 = master only.
    SetHeadphoneMix(f32),
    SetHeadphoneGain(f32),

    // ---------------------------------------------------------------- FX
    SetFx { deck: DeckId, slot: usize, kind: FxKind },
    SetFxActive { deck: DeckId, slot: usize, on: bool },
    SetFxWet { deck: DeckId, slot: usize, wet: f32 },
    SetFxBeats { deck: DeckId, slot: usize, beats: f32 },
    /// Parameter `index` (0 or 1) of the slot's effect, 0..1.
    SetFxParam { deck: DeckId, slot: usize, index: usize, value: f32 },

    // ---------------------------------------------------------------- mix playback / automix
    /// Start playing a mix through decks `schedule.decks` from mix time `start` (s).
    PlayMix { schedule: Box<MixSchedule>, start: f64 },
    MixSeek(f64),
    MixPause,
    MixResume,
    /// Stop mix playback (decks pause, automation is reset).
    MixStop,

    // ---------------------------------------------------------------- engine
    /// 2 or 4 decks.
    SetDeckCount(usize),
    /// Internal: attach / detach the recorder ring (use `EngineHandle::start_recording`).
    SetRecorder(Option<rtrb::Producer<f32>>),
}

impl Command {
    /// Deck addressed by this command, if any.
    pub fn deck(&self) -> Option<DeckId> {
        use Command::*;
        Some(match self {
            LoadTrack(d, _) | Unload(d) | Play(d) | Pause(d) | PlayPause(d) | Stop(d) | PlayStutter(d) | CueDown(d) | CueUp(d) | SetCue(d)
            | GotoCue(d) | Seek(d, _) | SetCueMode(d, _) | SetHotCueMode(d, _) | HotCueDown(d, _) | HotCueUp(d, _) | DeleteHotCue(d, _)
            | SetPitch(d, _) | SetTempo(d, _) | SetPitchRange(d, _) | PitchReset(d) | PitchBend(d, _) | SetKeylock(d, _) | SetKeyShift(d, _)
            | JogTouch(d, _) | Jog(d, _) | SetVinylMode(d, _) | SetReverse(d, _) | Brake(d, _) | Spinback(d) | SetSlip(d, _) | LoopIn(d)
            | LoopOut(d) | AutoLoop(d, _) | LoopExit(d) | Reloop(d) | LoopHalve(d) | LoopDouble(d) | LoopMove(d, _) | BeatJump(d, _)
            | SetQuantize(d, _) | Sync(d) | SetSync(d, _) | ToggleStemMute(d, _) | StemSolo(d, _) | Acapella(d) | Instrumental(d)
            | StemsReset(d) | StemEchoOut(d, _) | SetTrim(d, _) | SetEq(d, _, _) | SetFilter(d, _) | SetFilterResonance(d, _)
            | SetFader(d, _) | SetCrossfaderAssign(d, _) | SetPfl(d, _) | SetEchoSend(d, _) => *d,
            SetHotCue { deck, .. } | LoopRoll { deck, .. } | SetStemGain { deck, .. } | SetStemMute { deck, .. } | StemEcho { deck, .. }
            | SetFx { deck, .. } | SetFxActive { deck, .. } | SetFxWet { deck, .. } | SetFxBeats { deck, .. } | SetFxParam { deck, .. } => *deck,
            _ => return None,
        })
    }
}
