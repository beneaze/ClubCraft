//! User preferences (persisted as JSON in the ClubCraft data folder). The option set follows what
//! VirtualDJ (decks / mixer / display options) and DJ.Studio (analysis, transitions, export)
//! offer, limited to what ClubCraft actually implements.

use clubcraft_core::TransitionStyle;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum KeyNotation {
    #[default]
    Camelot,
    OpenKey,
    Musical,
}

/// Interface accent colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Accent {
    #[default]
    White,
    Pink,
    Blue,
    Green,
    Amber,
}

impl Accent {
    pub const ALL: [Accent; 5] = [Accent::White, Accent::Pink, Accent::Blue, Accent::Green, Accent::Amber];

    pub fn rgb(self) -> [u8; 3] {
        match self {
            Accent::White => [236, 238, 242],
            Accent::Pink => [255, 92, 140],
            Accent::Blue => [84, 148, 255],
            Accent::Green => [52, 208, 140],
            Accent::Amber => [255, 178, 48],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Accent::White => "White",
            Accent::Pink => "Pink",
            Accent::Blue => "Blue",
            Accent::Green => "Green",
            Accent::Amber => "Amber",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CueButton {
    /// CDJ: stopped sets/previews the cue, playing returns to it and stops.
    #[default]
    Cdj,
    /// Plays while held, returns on release.
    Hold,
    /// Cue-up-play: release starts playback.
    Cup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HotCuePress {
    /// While stopped, a hot cue previews while held.
    #[default]
    Preview,
    /// A hot cue always starts playback.
    Play,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LoadPosition {
    /// The first downbeat (Start cue).
    #[default]
    FirstBeat,
    /// The analysed mix-in point.
    MixIn,
    /// The very beginning of the file.
    FileStart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum XfCurve {
    #[default]
    Smooth,
    Full,
    Cut,
    Scratch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FaderLaw {
    Linear,
    #[default]
    Smooth,
    Fast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioFormat {
    Wav16,
    #[default]
    Wav24,
    Wav32Float,
}

impl AudioFormat {
    pub fn wav(self) -> clubcraft_engine::WavFormat {
        match self {
            AudioFormat::Wav16 => clubcraft_engine::WavFormat::Pcm16,
            AudioFormat::Wav24 => clubcraft_engine::WavFormat::Pcm24,
            AudioFormat::Wav32Float => clubcraft_engine::WavFormat::Float32,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            AudioFormat::Wav16 => "WAV 16-bit",
            AudioFormat::Wav24 => "WAV 24-bit",
            AudioFormat::Wav32Float => "WAV 32-bit float",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    // ------------------------------------------------------------ general / display
    pub key_notation: KeyNotation,
    /// UI zoom (1.0 = 100 %).
    pub ui_scale: f32,
    /// Open the home page on start (otherwise the last view).
    pub start_on_home: bool,
    pub accent: Accent,
    /// Preferences format; files older than 2 used the pink accent by default. Missing in old
    /// files, so it defaults to 0 there (not to `Prefs::default()`).
    #[serde(default)]
    pub version: u32,
    // ------------------------------------------------------------ decks
    pub cue_button: CueButton,
    pub hot_cue_press: HotCuePress,
    pub load_position: LoadPosition,
    pub pitch_range: u32,
    pub keylock: bool,
    pub quantize: bool,
    /// Sync a freshly loaded deck to the master deck.
    pub sync_on_load: bool,
    pub reset_eq_on_load: bool,
    pub reset_stems_on_load: bool,
    pub reset_key_on_load: bool,
    pub reset_pitch_on_load: bool,
    // ------------------------------------------------------------ mixer
    pub crossfader_curve: XfCurve,
    pub fader_curve: FaderLaw,
    /// Loudness normalisation of every track (auto-gain).
    pub auto_gain: bool,
    pub limiter: bool,
    pub limiter_ceiling_db: f32,
    // ------------------------------------------------------------ waveforms
    pub stem_waveforms: bool,
    /// Softer stem colours (otherwise the vivid palette).
    pub soft_stem_colors: bool,
    /// Seconds visible in the live scrolling waveforms.
    pub wave_zoom_secs: f64,
    // ------------------------------------------------------------ analysis & stems
    pub separate_stems: bool,
    /// Stem quality for playback: what decks, the studio and background separation use
    /// ("fast" | "high" | "ultra" | "best").
    pub stem_quality: String,
    /// Stem quality for exporting a mix: tracks with weaker stems are separated again first.
    pub export_stem_quality: String,
    pub deep_structure: bool,
    pub bpm_min: f64,
    pub bpm_max: f64,
    // ------------------------------------------------------------ studio defaults
    pub transition_style: TransitionStyle,
    /// Transition length in bars; `None` = choose from the track structure.
    pub transition_bars: Option<u32>,
    pub mix_keylock: bool,
    // ------------------------------------------------------------ export & recording
    pub export_format: AudioFormat,
    pub export_cue_sheet: bool,
    /// Extra seconds rendered after the end of the mix (echo / reverb tails).
    pub export_tail_secs: f64,
    pub record_format: AudioFormat,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            key_notation: KeyNotation::Camelot,
            ui_scale: 1.0,
            start_on_home: true,
            accent: Accent::White,
            version: 3,
            cue_button: CueButton::Cdj,
            hot_cue_press: HotCuePress::Preview,
            load_position: LoadPosition::FirstBeat,
            pitch_range: 8,
            keylock: true,
            quantize: true,
            sync_on_load: false,
            reset_eq_on_load: true,
            reset_stems_on_load: true,
            reset_key_on_load: true,
            reset_pitch_on_load: false,
            crossfader_curve: XfCurve::Smooth,
            fader_curve: FaderLaw::Smooth,
            auto_gain: true,
            limiter: true,
            limiter_ceiling_db: -0.5,
            stem_waveforms: true,
            soft_stem_colors: true,
            wave_zoom_secs: 8.0,
            separate_stems: false,
            stem_quality: if cfg!(target_os = "macos") { "best".into() } else { "high".into() },
            export_stem_quality: if cfg!(target_os = "macos") { "best".into() } else { "high".into() },
            deep_structure: false,
            bpm_min: 70.0,
            bpm_max: 180.0,
            transition_style: TransitionStyle::StemBlend,
            transition_bars: None,
            mix_keylock: true,
            export_format: AudioFormat::Wav24,
            export_cue_sheet: true,
            export_tail_secs: 2.0,
            record_format: AudioFormat::Wav24,
        }
    }
}

impl Prefs {
    pub fn path() -> std::path::PathBuf {
        clubcraft_core::paths::data_dir().join("preferences.json")
    }

    pub fn load() -> Prefs {
        let mut p: Prefs = std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if p.version < 2 {
            // white is the new default accent (it used to be pink)
            p.accent = Accent::White;
        }
        if p.version < 3 {
            // stems are separated on demand (transition button, export), not for every track in
            // the background: that kept the machine busy for hours
            p.separate_stems = false;
        }
        p.version = p.version.max(3);
        p
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(clubcraft_core::paths::data_dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let tmp = Self::path().with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(tmp, Self::path());
            }
        }
    }

    /// Engine-wide settings (mixer curves, auto-gain, limiter, cue modes for all decks).
    pub fn engine_commands(&self) -> Vec<clubcraft_engine::Command> {
        use clubcraft_engine::{Command as C, CrossfaderCurve, CueMode, FaderCurve, HotCueMode};
        let mut v = vec![
            C::SetCrossfaderCurve(match self.crossfader_curve {
                XfCurve::Smooth => CrossfaderCurve::Smooth,
                XfCurve::Full => CrossfaderCurve::Full,
                XfCurve::Cut => CrossfaderCurve::Cut,
                XfCurve::Scratch => CrossfaderCurve::Scratch,
            }),
            C::SetFaderCurve(match self.fader_curve {
                FaderLaw::Linear => FaderCurve::Linear,
                FaderLaw::Smooth => FaderCurve::Smooth,
                FaderLaw::Fast => FaderCurve::Fast,
            }),
            C::SetAutoGain(self.auto_gain),
            C::SetLimiter { enabled: self.limiter, ceiling_db: self.limiter_ceiling_db },
        ];
        for d in 0..2 {
            v.push(C::SetCueMode(d, match self.cue_button {
                CueButton::Cdj => CueMode::Cdj,
                CueButton::Hold => CueMode::Hold,
                CueButton::Cup => CueMode::Cup,
            }));
            v.push(C::SetHotCueMode(d, match self.hot_cue_press {
                HotCuePress::Preview => HotCueMode::Preview,
                HotCuePress::Play => HotCueMode::Play,
            }));
        }
        v
    }

    /// Commands applied to a deck right after a track was loaded.
    pub fn on_load_commands(&self, deck: usize, track: &clubcraft_engine::LoadedTrack) -> Vec<clubcraft_engine::Command> {
        use clubcraft_engine::{Command as C, EqBand, PitchRange};
        let mut v = vec![C::SetKeylock(deck, self.keylock), C::SetQuantize(deck, self.quantize)];
        if let Some(r) = PitchRange::from_percent(self.pitch_range) {
            v.push(C::SetPitchRange(deck, r));
        }
        if self.reset_eq_on_load {
            for b in [EqBand::Low, EqBand::Mid, EqBand::High] {
                v.push(C::SetEq(deck, b, 0.5));
            }
            v.push(C::SetFilter(deck, 0.5));
        }
        if self.reset_stems_on_load {
            v.push(C::StemsReset(deck));
        }
        if self.reset_key_on_load {
            v.push(C::SetKeyShift(deck, 0.0));
        }
        if self.reset_pitch_on_load {
            v.push(C::PitchReset(deck));
        }
        match self.load_position {
            LoadPosition::FirstBeat => {}
            LoadPosition::MixIn => v.push(C::Seek(deck, track.mix_in())),
            LoadPosition::FileStart => v.push(C::Seek(deck, 0.0)),
        }
        if self.sync_on_load {
            v.push(C::Sync(deck));
        }
        v
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn old_files_get_the_white_accent() {
        let old: Prefs = serde_json::from_str(r#"{"accent": "Pink", "ui_scale": 1.2}"#).unwrap();
        assert_eq!(old.version, 0);
        let new: Prefs = serde_json::from_str(r#"{"accent": "Pink", "version": 2}"#).unwrap();
        assert_eq!(new.accent, Accent::Pink);
    }
}
