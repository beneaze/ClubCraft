//! MIDI controller input: a small mapping layer from MIDI notes / CCs to engine [`Command`]s.
//!
//! Mappings are JSON files: a list of bindings, each a MIDI control (note / CC / pitch bend on a
//! channel), an action verb in VirtualDJ style and an optional deck:
//!
//! ```json
//! { "name": "My controller", "bindings": [
//!   { "control": { "type": "note", "channel": 0, "number": 11 }, "deck": 0, "action": "play_pause" },
//!   { "control": { "type": "note", "channel": 0, "number": 12 }, "deck": 0, "action": "cue" },
//!   { "control": { "type": "note", "channel": 4, "number": 2 },  "deck": 0, "action": "hot_cue 3" },
//!   { "control": { "type": "cc",   "channel": 0, "number": 7 },  "deck": 0, "action": "eq_low" },
//!   { "control": { "type": "cc",   "channel": 0, "number": 33 }, "deck": 0, "action": "jog", "encoding": "relative2c" },
//!   { "control": { "type": "note", "channel": 0, "number": 54 }, "deck": 0, "action": "jog_touch" },
//!   { "control": { "type": "cc",   "channel": 6, "number": 31 }, "action": "crossfader" }
//! ] }
//! ```
//!
//! Verbs (args in brackets): `play_pause play pause stop cue set_cue goto_cue sync sync_off
//! keylock slip reverse quantize pfl censor`, `hot_cue N`, `delete_cue N`, `loop N` (beats),
//! `loop_roll N`, `loop_in loop_out loop_exit reloop loop_half loop_double`, `loop_move ±N`,
//! `beatjump ±N`, `pitch`, `pitch_bend ±1`, `pitch_reset`, `pitch_range`, `key_move ±N`,
//! `key_reset`, `jog`, `jog_touch`, `brake [beats]`, `spinback`,
//! `stem <vocal|melody|bass|drums> [mute|solo|volume|echo_out]`, `acapella`, `instrumental`,
//! `stems_reset`, `eq_low eq_mid eq_high filter volume gain`, `crossfader master_volume
//! headphone_mix headphone_volume`, `fx N` (toggle slot N, 1-based), `fx_wet N`.
//!
//! Learn mode: [`MidiMapper::learn`] binds the next incoming control to an action.

use crate::command::{Command, DeckId, EqBand};
use crate::engine::CommandSender;
use crate::state::EngineState;
use anyhow::{anyhow, bail, Context, Result};
use clubcraft_core::StemKind;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// A physical control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MidiControl {
    Note { channel: u8, number: u8 },
    Cc { channel: u8, number: u8 },
    PitchBend { channel: u8 },
}

/// How CC values are interpreted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CcEncoding {
    /// 0..127 absolute.
    #[default]
    Absolute,
    /// Relative, two's complement (1 = +1, 127 = −1).
    Relative2c,
    /// Relative, offset binary (65 = +1, 63 = −1).
    RelativeOffset,
    /// Relative, sign bit (1 = +1, 65 = −1).
    RelativeSignBit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub control: MidiControl,
    pub action: String,
    #[serde(default)]
    pub deck: Option<DeckId>,
    #[serde(default)]
    pub encoding: CcEncoding,
    #[serde(default)]
    pub invert: bool,
    /// Jog ticks per platter revolution (relative encoders).
    #[serde(default = "default_ticks")]
    pub ticks_per_rev: f64,
}

fn default_ticks() -> f64 {
    128.0
}

/// A controller mapping.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MidiMapping {
    pub name: String,
    /// MIDI port name (substring) this mapping is meant for.
    #[serde(default)]
    pub device: Option<String>,
    pub bindings: Vec<Binding>,
}

impl MidiMapping {
    pub fn from_json(s: &str) -> Result<Self> {
        let m: MidiMapping = serde_json::from_str(s)?;
        for b in &m.bindings {
            parse_action(&b.action).with_context(|| format!("binding {:?}", b.action))?;
        }
        Ok(m)
    }

    pub fn load(path: &Path) -> Result<Self> {
        Self::from_json(&std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// Parsed action verb.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    PlayPause,
    Play,
    Pause,
    Stop,
    Cue,
    SetCue,
    GotoCue,
    Sync,
    SyncOff,
    Keylock,
    Slip,
    Reverse,
    Censor,
    Quantize,
    Pfl,
    HotCue(usize),
    DeleteCue(usize),
    Loop(f64),
    LoopRoll(f64),
    LoopIn,
    LoopOut,
    LoopExit,
    Reloop,
    LoopHalf,
    LoopDouble,
    LoopMove(f64),
    BeatJump(f64),
    Pitch,
    PitchBend(f32),
    PitchReset,
    PitchRangeNext,
    KeyMove(f32),
    KeyReset,
    Jog,
    JogTouch,
    Brake(f64),
    Spinback,
    StemMute(StemKind),
    StemSolo(StemKind),
    StemVolume(StemKind),
    StemEchoOut(StemKind),
    Acapella,
    Instrumental,
    StemsReset,
    EqLow,
    EqMid,
    EqHigh,
    Filter,
    Volume,
    Gain,
    Crossfader,
    MasterVolume,
    HeadphoneMix,
    HeadphoneVolume,
    FxToggle(usize),
    FxWet(usize),
}

impl Action {
    /// Continuous controls (faders/knobs) vs buttons.
    pub fn is_continuous(self) -> bool {
        use Action::*;
        matches!(
            self,
            Pitch | StemVolume(_) | EqLow | EqMid | EqHigh | Filter | Volume | Gain | Crossfader | MasterVolume | HeadphoneMix | HeadphoneVolume | FxWet(_) | Jog
        )
    }
}

fn stem_name(s: &str) -> Result<StemKind> {
    Ok(match s.trim_matches(|c| c == '\'' || c == '"').to_ascii_lowercase().as_str() {
        "vocal" | "vocals" | "voc" => StemKind::Vocals,
        "melody" | "instru" | "other" | "mel" => StemKind::Other,
        "bass" => StemKind::Bass,
        "drums" | "rhythm" | "kick" | "beat" => StemKind::Drums,
        x => bail!("unknown stem {x:?}"),
    })
}

fn num(s: Option<&str>) -> Result<f64> {
    let s = s.ok_or_else(|| anyhow!("missing number"))?;
    if let Some((a, b)) = s.split_once('/') {
        return Ok(a.trim_start_matches('+').parse::<f64>()? / b.parse::<f64>()?);
    }
    Ok(s.trim_start_matches('+').trim_end_matches("bt").parse()?)
}

/// Parse a verb string such as `"hot_cue 3"` or `"stem vocal mute"`.
pub fn parse_action(s: &str) -> Result<Action> {
    use Action::*;
    let mut it = s.split_whitespace();
    let verb = it.next().ok_or_else(|| anyhow!("empty action"))?.to_ascii_lowercase();
    let a1 = it.next();
    let a2 = it.next();
    Ok(match verb.as_str() {
        "play_pause" => PlayPause,
        "play" => Play,
        "pause" => Pause,
        "stop" => Stop,
        "cue" | "cue_button" => Cue,
        "set_cue" => SetCue,
        "goto_cue" => GotoCue,
        "sync" => Sync,
        "sync_off" => SyncOff,
        "keylock" | "master_tempo" | "key_lock" => Keylock,
        "slip" | "slip_mode" => Slip,
        "reverse" => Reverse,
        "censor" => Censor,
        "quantize" => Quantize,
        "pfl" => Pfl,
        "hot_cue" => HotCue(num(a1)? as usize),
        "delete_cue" => DeleteCue(num(a1)? as usize),
        "loop" => Loop(num(a1).unwrap_or(4.0)),
        "loop_roll" => LoopRoll(num(a1).unwrap_or(0.25)),
        "loop_in" => LoopIn,
        "loop_out" => LoopOut,
        "loop_exit" => LoopExit,
        "reloop" | "reloop_exit" => Reloop,
        "loop_half" => LoopHalf,
        "loop_double" => LoopDouble,
        "loop_move" => LoopMove(num(a1)?),
        "beatjump" => BeatJump(num(a1)?),
        "pitch" => Pitch,
        "pitch_bend" => PitchBend(num(a1)? as f32),
        "pitch_reset" => PitchReset,
        "pitch_range" => PitchRangeNext,
        "key_move" => KeyMove(num(a1)? as f32),
        "key_reset" => KeyReset,
        "jog" | "jogwheel" => Jog,
        "jog_touch" | "touchwheel_touch" => JogTouch,
        "brake" => Brake(num(a1).unwrap_or(2.0)),
        "spinback" | "backspin" => Spinback,
        "stem" | "stem_pad" | "mute_stem" | "only_stem" | "stem_volume" => {
            let (name, mode) = match verb.as_str() {
                "mute_stem" => (a1, Some("mute")),
                "only_stem" => (a1, Some("solo")),
                "stem_volume" => (a1, Some("volume")),
                _ if a1 == Some("isolate") => (a2, Some("solo")),
                _ => (a1, a2),
            };
            let name = name.ok_or_else(|| anyhow!("stem name missing"))?;
            match name {
                "acapella" | "acappella" => Acapella,
                "instrumental" => Instrumental,
                _ => {
                    let st = stem_name(name)?;
                    match mode.unwrap_or("mute") {
                        "mute" => StemMute(st),
                        "solo" | "isolate" | "only" => StemSolo(st),
                        "volume" => StemVolume(st),
                        "echo_out" | "echo" => StemEchoOut(st),
                        m => bail!("unknown stem mode {m:?}"),
                    }
                }
            }
        }
        "acapella" => Acapella,
        "instrumental" => Instrumental,
        "stems_reset" => StemsReset,
        "eq_low" => EqLow,
        "eq_mid" => EqMid,
        "eq_high" => EqHigh,
        "filter" => Filter,
        "volume" | "level" => Volume,
        "gain" => Gain,
        "crossfader" => Crossfader,
        "master_volume" => MasterVolume,
        "headphone_mix" => HeadphoneMix,
        "headphone_volume" => HeadphoneVolume,
        "fx" | "effect_active" => FxToggle((num(a1)? as usize).max(1) - 1),
        "fx_wet" => FxWet((num(a1)? as usize).max(1) - 1),
        v => bail!("unknown action {v:?}"),
    })
}

/// A decoded MIDI message.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiEvent {
    pub control: MidiControl,
    /// 0..1 for absolute values (note velocity, CC, pitch bend); raw 0..127 in `raw`.
    pub value: f32,
    pub raw: u16,
    /// Button pressed (note on with velocity > 0, CC > 0).
    pub pressed: bool,
}

pub fn decode(msg: &[u8]) -> Option<MidiEvent> {
    let st = *msg.first()?;
    let ch = st & 0x0F;
    match st & 0xF0 {
        0x90 | 0x80 => {
            let n = *msg.get(1)?;
            let v = *msg.get(2).unwrap_or(&0);
            let on = st & 0xF0 == 0x90 && v > 0;
            Some(MidiEvent { control: MidiControl::Note { channel: ch, number: n }, value: v as f32 / 127.0, raw: v as u16, pressed: on })
        }
        0xB0 => {
            let n = *msg.get(1)?;
            let v = *msg.get(2)?;
            Some(MidiEvent { control: MidiControl::Cc { channel: ch, number: n }, value: v as f32 / 127.0, raw: v as u16, pressed: v > 0 })
        }
        0xE0 => {
            let v = (*msg.get(1)? as u16) | ((*msg.get(2)? as u16) << 7);
            Some(MidiEvent { control: MidiControl::PitchBend { channel: ch }, value: v as f32 / 16383.0, raw: v, pressed: true })
        }
        _ => None,
    }
}

fn relative(raw: u16, enc: CcEncoding) -> f64 {
    let v = raw as i32;
    (match enc {
        CcEncoding::Absolute | CcEncoding::Relative2c => {
            if v >= 64 {
                v - 128
            } else {
                v
            }
        }
        CcEncoding::RelativeOffset => v - 64,
        CcEncoding::RelativeSignBit => {
            if v & 0x40 != 0 {
                -(v & 0x3F)
            } else {
                v
            }
        }
    }) as f64
}

/// Result of learn mode.
#[derive(Clone, Debug, PartialEq)]
pub struct LearnEvent {
    pub binding: Binding,
}

/// Translates MIDI messages into commands using a [`MidiMapping`].
pub struct MidiMapper {
    pub mapping: MidiMapping,
    parsed: Vec<Action>,
    learn: Option<(String, Option<DeckId>)>,
    learned: Option<LearnEvent>,
    state: EngineState,
}

impl MidiMapper {
    pub fn new(mapping: MidiMapping) -> Result<Self> {
        let parsed = mapping.bindings.iter().map(|b| parse_action(&b.action)).collect::<Result<Vec<_>>>()?;
        Ok(MidiMapper { mapping, parsed, learn: None, learned: None, state: EngineState::default() })
    }

    /// Give the mapper the latest engine state (used for toggles like keylock / pfl).
    pub fn update_state(&mut self, s: &EngineState) {
        self.state = *s;
    }

    /// Bind the next incoming control to `action` (for `deck`).
    pub fn learn(&mut self, action: &str, deck: Option<DeckId>) -> Result<()> {
        parse_action(action)?;
        self.learn = Some((action.to_string(), deck));
        Ok(())
    }

    pub fn cancel_learn(&mut self) {
        self.learn = None;
    }

    pub fn is_learning(&self) -> bool {
        self.learn.is_some()
    }

    /// The binding created by learn mode, once.
    pub fn take_learned(&mut self) -> Option<LearnEvent> {
        self.learned.take()
    }

    /// Handle one raw MIDI message, appending the resulting commands.
    pub fn handle(&mut self, msg: &[u8], out: &mut Vec<Command>) {
        let Some(ev) = decode(msg) else { return };
        if let Some((action, deck)) = self.learn.clone() {
            if ev.pressed {
                let a = parse_action(&action).unwrap_or(Action::PlayPause);
                let encoding = if a == Action::Jog { CcEncoding::Relative2c } else { CcEncoding::Absolute };
                let b = Binding { control: ev.control, action, deck, encoding, invert: false, ticks_per_rev: default_ticks() };
                let keep: Vec<bool> = self.mapping.bindings.iter().map(|x| !(x.control == ev.control && x.deck == deck)).collect();
                let mut i = 0;
                self.mapping.bindings.retain(|_| {
                    i += 1;
                    keep[i - 1]
                });
                let mut j = 0;
                self.parsed.retain(|_| {
                    j += 1;
                    keep[j - 1]
                });
                self.mapping.bindings.push(b.clone());
                self.parsed.push(a);
                self.learned = Some(LearnEvent { binding: b });
                self.learn = None;
            }
            return;
        }
        for (b, a) in self.mapping.bindings.iter().zip(self.parsed.iter()) {
            if b.control != ev.control {
                continue;
            }
            let d = b.deck.unwrap_or(0).min(crate::state::MAX_DECKS - 1);
            let v = if b.invert { 1.0 - ev.value } else { ev.value };
            Self::dispatch(&self.state, *a, d, b, &ev, v, out);
        }
    }

    fn dispatch(st: &EngineState, a: Action, d: DeckId, b: &Binding, ev: &MidiEvent, v: f32, out: &mut Vec<Command>) {
        use Action as A;
        use Command as C;
        let ds = st.deck(d);
        let press = ev.pressed;
        match a {
            A::PlayPause if press => out.push(C::PlayPause(d)),
            A::Play if press => out.push(C::Play(d)),
            A::Pause if press => out.push(C::Pause(d)),
            A::Stop if press => out.push(C::Stop(d)),
            A::Cue => out.push(if press { C::CueDown(d) } else { C::CueUp(d) }),
            A::SetCue if press => out.push(C::SetCue(d)),
            A::GotoCue if press => out.push(C::GotoCue(d)),
            A::Sync if press => out.push(if ds.sync { C::SetSync(d, false) } else { C::Sync(d) }),
            A::SyncOff if press => out.push(C::SetSync(d, false)),
            A::Keylock if press => out.push(C::SetKeylock(d, !ds.keylock)),
            A::Slip if press => out.push(C::SetSlip(d, !ds.slip_mode)),
            A::Reverse if press => out.push(C::SetReverse(d, !ds.reverse)),
            A::Censor => out.push(C::SetReverse(d, press)),
            A::Quantize if press => out.push(C::SetQuantize(d, !ds.quantize)),
            A::Pfl if press => out.push(C::SetPfl(d, !ds.pfl)),
            A::HotCue(n) => {
                let i = n.max(1) - 1;
                out.push(if press { C::HotCueDown(d, i) } else { C::HotCueUp(d, i) })
            }
            A::DeleteCue(n) if press => out.push(C::DeleteHotCue(d, n.max(1) - 1)),
            A::Loop(beats) if press => out.push(C::AutoLoop(d, beats)),
            A::LoopRoll(beats) => out.push(C::LoopRoll { deck: d, beats, on: press }),
            A::LoopIn if press => out.push(C::LoopIn(d)),
            A::LoopOut if press => out.push(C::LoopOut(d)),
            A::LoopExit if press => out.push(C::LoopExit(d)),
            A::Reloop if press => out.push(C::Reloop(d)),
            A::LoopHalf if press => out.push(C::LoopHalve(d)),
            A::LoopDouble if press => out.push(C::LoopDouble(d)),
            A::LoopMove(x) if press => out.push(C::LoopMove(d, x)),
            A::BeatJump(x) if press => out.push(C::BeatJump(d, x)),
            A::Pitch => out.push(C::SetPitch(d, v * 2.0 - 1.0)),
            A::PitchBend(x) => out.push(C::PitchBend(d, if press { x } else { 0.0 })),
            A::PitchReset if press => out.push(C::PitchReset(d)),
            A::PitchRangeNext if press => out.push(C::SetPitchRange(d, ds.pitch_range.next())),
            A::KeyMove(x) if press => out.push(C::SetKeyShift(d, ds.key_shift + x)),
            A::KeyReset if press => out.push(C::SetKeyShift(d, 0.0)),
            A::Jog => {
                let ticks = relative(ev.raw, if b.encoding == CcEncoding::Absolute { CcEncoding::Relative2c } else { b.encoding });
                let ticks = if b.invert { -ticks } else { ticks };
                out.push(C::Jog(d, ticks / b.ticks_per_rev.max(1.0)));
            }
            A::JogTouch => out.push(C::JogTouch(d, press)),
            A::Brake(beats) if press => out.push(C::Brake(d, beats)),
            A::Spinback if press => out.push(C::Spinback(d)),
            A::StemMute(s) if press => out.push(C::ToggleStemMute(d, s)),
            A::StemSolo(s) if press => out.push(C::StemSolo(d, s)),
            A::StemVolume(s) => out.push(C::SetStemGain { deck: d, stem: s, gain: v }),
            A::StemEchoOut(s) if press => out.push(C::StemEchoOut(d, s)),
            A::Acapella if press => out.push(C::Acapella(d)),
            A::Instrumental if press => out.push(C::Instrumental(d)),
            A::StemsReset if press => out.push(C::StemsReset(d)),
            A::EqLow => out.push(C::SetEq(d, EqBand::Low, v)),
            A::EqMid => out.push(C::SetEq(d, EqBand::Mid, v)),
            A::EqHigh => out.push(C::SetEq(d, EqBand::High, v)),
            A::Filter => out.push(C::SetFilter(d, v)),
            A::Volume => out.push(C::SetFader(d, v)),
            A::Gain => out.push(C::SetTrim(d, (v - 0.5) * 24.0)),
            A::Crossfader => out.push(C::SetCrossfader(v)),
            A::MasterVolume => out.push(C::SetMasterGain(crate::dsp::gain_to_db(v * v).max(-60.0))),
            A::HeadphoneMix => out.push(C::SetHeadphoneMix(v)),
            A::HeadphoneVolume => out.push(C::SetHeadphoneGain(crate::dsp::gain_to_db(v * v).max(-60.0))),
            A::FxToggle(slot) if press => {
                let on = !ds.fx.get(slot).map(|f| f.active).unwrap_or(false);
                out.push(C::SetFxActive { deck: d, slot, on })
            }
            A::FxWet(slot) => out.push(C::SetFxWet { deck: d, slot, wet: v }),
            _ => {}
        }
    }
}

/// Names of the available MIDI input ports.
pub fn input_ports() -> Result<Vec<String>> {
    let mi = midir::MidiInput::new("ClubCraft")?;
    Ok(mi.ports().iter().filter_map(|p| mi.port_name(p).ok()).collect())
}

/// An open MIDI input. Dropping it closes the port.
pub struct MidiConnection {
    _conn: midir::MidiInputConnection<()>,
    pub port: String,
}

/// Open the first input port whose name contains `port` and route its messages through
/// `mapper` to the engine.
pub fn connect(port: &str, mapper: Arc<Mutex<MidiMapper>>, sender: CommandSender) -> Result<MidiConnection> {
    let mut mi = midir::MidiInput::new("ClubCraft")?;
    mi.ignore(midir::Ignore::All);
    let ports = mi.ports();
    let p = ports
        .iter()
        .find(|p| mi.port_name(p).map(|n| n.contains(port)).unwrap_or(false))
        .ok_or_else(|| anyhow!("MIDI port {port:?} not found"))?
        .clone();
    let name = mi.port_name(&p)?;
    let conn = mi
        .connect(
            &p,
            "clubcraft-in",
            move |_ts, msg, _| {
                let mut cmds = Vec::new();
                mapper.lock().handle(msg, &mut cmds);
                for c in cmds {
                    if sender.send(c).is_err() {
                        log::warn!("engine queue full, MIDI command dropped");
                    }
                }
            },
            (),
        )
        .map_err(|e| anyhow!("MIDI connect: {e}"))?;
    Ok(MidiConnection { _conn: conn, port: name })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r#"{ "name": "test", "bindings": [
        { "control": { "type": "note", "channel": 0, "number": 11 }, "deck": 1, "action": "play_pause" },
        { "control": { "type": "note", "channel": 0, "number": 20 }, "deck": 0, "action": "hot_cue 3" },
        { "control": { "type": "note", "channel": 0, "number": 21 }, "deck": 0, "action": "stem vocal mute" },
        { "control": { "type": "cc", "channel": 0, "number": 7 }, "deck": 0, "action": "eq_low" },
        { "control": { "type": "cc", "channel": 0, "number": 33 }, "deck": 0, "action": "jog", "encoding": "relative2c" },
        { "control": { "type": "cc", "channel": 6, "number": 31 }, "action": "crossfader" },
        { "control": { "type": "note", "channel": 0, "number": 22 }, "deck": 0, "action": "loop 1/2" }
    ] }"#;

    #[test]
    fn mapping_dispatch() {
        let mut m = MidiMapper::new(MidiMapping::from_json(MAP).unwrap()).unwrap();
        let mut out = Vec::new();
        m.handle(&[0x90, 11, 127], &mut out);
        assert!(matches!(out[0], Command::PlayPause(1)));
        out.clear();
        m.handle(&[0x90, 20, 100], &mut out);
        m.handle(&[0x80, 20, 0], &mut out);
        assert!(matches!(out[0], Command::HotCueDown(0, 2)));
        assert!(matches!(out[1], Command::HotCueUp(0, 2)));
        out.clear();
        m.handle(&[0xB0, 7, 0], &mut out);
        assert!(matches!(out[0], Command::SetEq(0, EqBand::Low, v) if v == 0.0));
        out.clear();
        m.handle(&[0xB0, 33, 127], &mut out);
        assert!(matches!(out[0], Command::Jog(0, r) if (r + 1.0 / 128.0).abs() < 1e-9));
        out.clear();
        m.handle(&[0xB6, 31, 127], &mut out);
        assert!(matches!(out[0], Command::SetCrossfader(v) if v == 1.0));
        out.clear();
        m.handle(&[0x90, 21, 127], &mut out);
        assert!(matches!(out[0], Command::ToggleStemMute(0, StemKind::Vocals)));
        out.clear();
        m.handle(&[0x90, 22, 127], &mut out);
        assert!(matches!(out[0], Command::AutoLoop(0, b) if b == 0.5));
    }

    #[test]
    fn learn_mode() {
        let mut m = MidiMapper::new(MidiMapping::default()).unwrap();
        m.learn("sync", Some(2)).unwrap();
        let mut out = Vec::new();
        m.handle(&[0x91, 40, 127], &mut out);
        assert!(out.is_empty());
        let l = m.take_learned().unwrap();
        assert_eq!(l.binding.control, MidiControl::Note { channel: 1, number: 40 });
        m.handle(&[0x91, 40, 127], &mut out);
        assert!(matches!(out[0], Command::Sync(2)));
        assert!(parse_action("stem bass solo").is_ok());
        assert!(parse_action("frobnicate").is_err());
    }
}
