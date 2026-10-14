//! View-model of the live decks: what the UI draws (`DeckUi`, `MixerUi`) and what it asks for
//! (`LiveAction`). The engine adapter translates between this and `clubcraft_engine`.

use clubcraft_core::{BeatGrid, Cue, Key, Section, TrackId, Waveform};
use std::sync::Arc;

#[derive(Clone)]
pub struct DeckUi {
    pub loaded: bool,
    pub track_id: Option<TrackId>,
    pub title: String,
    pub artist: String,
    pub duration: f64,
    pub position: f64,
    pub playing: bool,
    /// Track's own BPM and the current playing BPM.
    pub base_bpm: Option<f64>,
    pub bpm: Option<f64>,
    pub pitch_pct: f32,
    pub pitch_range: f32,
    pub keylock: bool,
    pub key: Option<Key>,
    pub key_shift: i32,
    pub synced: bool,
    pub master: bool,
    pub slip: bool,
    pub quantize: bool,
    pub cue_point: f64,
    pub loop_region: Option<(f64, f64)>,
    pub loop_active: bool,
    pub auto_loop_beats: f64,
    /// 0..1 per stem; how much of the track has been separated (0..1).
    pub stem_gains: [f32; 4],
    pub stems_ready: f32,
    pub has_stems: bool,
    pub eq: [f32; 3],
    pub filter: f32,
    pub gain: f32,
    pub fader: f32,
    /// Automix volume on top of the fader (1 = none).
    pub auto_volume: f32,
    pub pfl: bool,
    pub vu: [f32; 2],
    pub fx: [FxUi; 3],
    pub waveform: Option<Arc<Waveform>>,
    pub grid: Option<BeatGrid>,
    pub cues: Vec<Cue>,
    pub sections: Vec<Section>,
    /// Beat distance to master deck (-0.5..0.5), for the phase meter.
    pub phase_offset: Option<f64>,
}

impl Default for DeckUi {
    fn default() -> Self {
        DeckUi {
            loaded: false,
            track_id: None,
            title: String::new(),
            artist: String::new(),
            duration: 0.0,
            position: 0.0,
            playing: false,
            base_bpm: None,
            bpm: None,
            pitch_pct: 0.0,
            pitch_range: 8.0,
            keylock: true,
            key: None,
            key_shift: 0,
            synced: false,
            master: false,
            slip: false,
            quantize: true,
            cue_point: 0.0,
            loop_region: None,
            loop_active: false,
            auto_loop_beats: 4.0,
            stem_gains: [1.0; 4],
            stems_ready: 0.0,
            has_stems: false,
            eq: [0.5; 3],
            filter: 0.5,
            gain: 0.5,
            fader: 1.0,
            auto_volume: 1.0,
            pfl: false,
            vu: [0.0; 2],
            fx: [FxUi { kind: 0, on: false, amount: 0.5, beats: 0.5 }, FxUi { kind: 2, on: false, amount: 0.5, beats: 1.0 }, FxUi { kind: 3, on: false, amount: 0.5, beats: 1.0 }],
            waveform: None,
            grid: None,
            cues: Vec::new(),
            sections: Vec::new(),
            phase_offset: None,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct FxUi {
    pub kind: usize,
    pub on: bool,
    pub amount: f32,
    pub beats: f32,
}

pub const FX_NAMES: [&str; 9] = ["Echo", "Ping Pong", "Reverb", "Flanger", "Filter LFO", "Gate", "Roll", "Bitcrusher", "Brake"];

#[derive(Clone)]
pub struct MixerUi {
    pub crossfader: f32,
    pub master: f32,
    pub headphone_mix: f32,
    pub headphone_vol: f32,
    pub master_vu: [f32; 2],
    pub recording: bool,
    pub record_secs: f64,
    pub master_bpm: Option<f64>,
    pub automix: bool,
    pub output_name: String,
    pub cpu: f32,
}

/// Stem helpers that map onto stem gains.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StemPreset {
    All,
    Acapella,
    Instrumental,
    DrumsOnly,
    NoDrums,
}

impl StemPreset {
    pub fn gains(self) -> [f32; 4] {
        match self {
            StemPreset::All => [1.0; 4],
            StemPreset::Acapella => [0.0, 0.0, 0.0, 1.0],
            StemPreset::Instrumental => [1.0, 1.0, 1.0, 0.0],
            StemPreset::DrumsOnly => [1.0, 0.0, 0.0, 0.0],
            StemPreset::NoDrums => [0.0, 1.0, 1.0, 1.0],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LiveAction {
    Load(usize, TrackId),
    Eject(usize),
    PlayPause(usize),
    /// Cue button pressed / released (CDJ style: hold to preview when stopped).
    CueDown(usize),
    CueUp(usize),
    Seek(usize, f64),
    /// Nudge / scratch by seconds (waveform drag).
    Scrub(usize, f64),
    ScrubEnd(usize),
    Sync(usize),
    SetMaster(usize),
    Pitch(usize, f32),
    PitchRange(usize, f32),
    PitchBend(usize, f32),
    Keylock(usize, bool),
    KeyShift(usize, i32),
    MatchKey(usize),
    HotCue(usize, u8),
    HotCueRelease(usize, u8),
    DeleteHotCue(usize, u8),
    AutoLoop(usize, f64),
    LoopToggle(usize),
    LoopHalve(usize),
    LoopDouble(usize),
    LoopIn(usize),
    LoopOut(usize),
    LoopRoll(usize, f64, bool),
    BeatJump(usize, f64),
    Slip(usize, bool),
    Quantize(usize, bool),
    StemGain(usize, usize, f32),
    StemPreset(usize, StemPreset),
    StemSolo(usize, usize),
    Eq(usize, usize, f32),
    Filter(usize, f32),
    Gain(usize, f32),
    Fader(usize, f32),
    Pfl(usize, bool),
    Fx(usize, usize, FxUi),
    Crossfader(f32),
    MasterVol(f32),
    HeadphoneMix(f32),
    HeadphoneVol(f32),
    Record(bool),
    Automix(bool),
    AutomixQueue(Vec<TrackId>),
}

impl Default for MixerUi {
    fn default() -> Self {
        MixerUi { crossfader: 0.0, master: 0.8, headphone_mix: 0.0, headphone_vol: 0.7, master_vu: [0.0; 2], recording: false, record_secs: 0.0, master_bpm: None, automix: false, output_name: String::new(), cpu: 0.0 }
    }
}
