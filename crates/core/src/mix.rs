//! Studio mixes: an ordered list of tracks joined by beat-matched transitions, each with
//! automation lanes (volume, per-stem gains, EQ, filter, echo). The same model drives the
//! offline renderer, the studio preview and the live-deck automix.

use crate::analysis::StemKind;
use crate::library::TrackId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    /// The outgoing track.
    Out,
    /// The incoming track.
    In,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Param {
    /// Channel fader, 0..1 (linear amplitude via fader curve).
    Volume,
    /// Stem gain 0..1.
    Stem(StemKind),
    /// EQ band gain, 0..1 where 0.5 = flat, 0 = kill, 1 = +6 dB.
    EqLow,
    EqMid,
    EqHigh,
    /// Single-knob filter, 0..1 where 0.5 = off, < 0.5 low-pass, > 0.5 high-pass.
    Filter,
    /// Echo send 0..1 (beat-synced 1/2 beat echo, feedback rises with the value).
    Echo,
}

impl Param {
    pub fn neutral(self) -> f32 {
        match self {
            Param::Volume | Param::Stem(_) => 1.0,
            Param::EqLow | Param::EqMid | Param::EqHigh | Param::Filter => 0.5,
            Param::Echo => 0.0,
        }
    }

    pub fn label(self) -> String {
        match self {
            Param::Volume => "Volume".into(),
            Param::Stem(s) => s.label().into(),
            Param::EqLow => "Low".into(),
            Param::EqMid => "Mid".into(),
            Param::EqHigh => "High".into(),
            Param::Filter => "Filter".into(),
            Param::Echo => "Echo".into(),
        }
    }
}

/// Automation curve over a transition. `x` is the position in beats from the transition start
/// (0..=length_beats), `y` the parameter value. Linear interpolation between points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    pub side: Side,
    pub param: Param,
    pub points: Vec<(f32, f32)>,
}

impl Lane {
    pub fn new(side: Side, param: Param, points: &[(f32, f32)]) -> Self {
        Lane { side, param, points: points.to_vec() }
    }

    pub fn value_at(&self, x: f32) -> f32 {
        let p = &self.points;
        match p.len() {
            0 => self.param.neutral(),
            1 => p[0].1,
            _ => {
                if x <= p[0].0 {
                    return p[0].1;
                }
                for w in p.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    if x <= b.0 {
                        let t = if b.0 > a.0 { (x - a.0) / (b.0 - a.0) } else { 1.0 };
                        return a.1 + (b.1 - a.1) * t;
                    }
                }
                p[p.len() - 1].1
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransitionStyle {
    /// Equal-power volume crossfade.
    Crossfade,
    /// Classic DJ blend: swap the low EQ at the phrase midpoint, fade highs/mids.
    BassSwap,
    /// Stem-aware blend: drums & bass of the incoming track come in while the outgoing vocals
    /// are removed first, then melody, then the rhythm section is swapped on the downbeat.
    StemBlend,
    /// Outgoing vocals keep playing over the incoming instrumental (mashup style).
    VocalOver,
    /// High-pass filter sweep out of the outgoing track.
    FilterOut,
    /// Echo out of the outgoing track, incoming starts on the downbeat.
    EchoOut,
    /// Hard cut on the downbeat.
    Cut,
    /// Lanes edited by hand.
    Custom,
}

impl TransitionStyle {
    pub const ALL: [TransitionStyle; 8] = [
        TransitionStyle::StemBlend,
        TransitionStyle::BassSwap,
        TransitionStyle::Crossfade,
        TransitionStyle::VocalOver,
        TransitionStyle::FilterOut,
        TransitionStyle::EchoOut,
        TransitionStyle::Cut,
        TransitionStyle::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TransitionStyle::Crossfade => "Crossfade",
            TransitionStyle::BassSwap => "Bass swap",
            TransitionStyle::StemBlend => "Stem blend",
            TransitionStyle::VocalOver => "Vocal over",
            TransitionStyle::FilterOut => "Filter out",
            TransitionStyle::EchoOut => "Echo out",
            TransitionStyle::Cut => "Cut",
            TransitionStyle::Custom => "Custom",
        }
    }

    /// Default automation lanes for a transition of `len` beats.
    pub fn lanes(self, len: f32) -> Vec<Lane> {
        use Param::*;
        use Side::*;
        let h = len * 0.5;
        let q = len * 0.25;
        match self {
            TransitionStyle::Crossfade => vec![
                Lane::new(Out, Volume, &[(0.0, 1.0), (h, 0.85), (len, 0.0)]),
                Lane::new(In, Volume, &[(0.0, 0.0), (h, 0.85), (len, 1.0)]),
            ],
            TransitionStyle::BassSwap => vec![
                Lane::new(In, EqLow, &[(0.0, 0.0), (h - 0.01, 0.0), (h, 0.5)]),
                Lane::new(Out, EqLow, &[(0.0, 0.5), (h - 0.01, 0.5), (h, 0.0)]),
                Lane::new(In, Volume, &[(0.0, 0.0), (q, 0.8), (h, 1.0)]),
                Lane::new(Out, Volume, &[(0.0, 1.0), (h, 1.0), (len, 0.0)]),
                Lane::new(Out, EqHigh, &[(0.0, 0.5), (len, 0.2)]),
            ],
            TransitionStyle::StemBlend => vec![
                // incoming: drums first, bass on the swap, melody, vocals last
                Lane::new(In, Volume, &[(0.0, 1.0)]),
                Lane::new(In, Stem(StemKind::Drums), &[(0.0, 0.0), (q, 1.0)]),
                Lane::new(In, Stem(StemKind::Bass), &[(0.0, 0.0), (h - 0.01, 0.0), (h, 1.0)]),
                Lane::new(In, Stem(StemKind::Other), &[(0.0, 0.0), (q, 0.3), (len * 0.75, 1.0)]),
                Lane::new(In, Stem(StemKind::Vocals), &[(0.0, 0.0), (len * 0.75, 0.0), (len, 1.0)]),
                // outgoing: vocals leave early, bass swapped, drums and melody fade
                Lane::new(Out, Stem(StemKind::Vocals), &[(0.0, 1.0), (q, 0.0)]),
                Lane::new(Out, Stem(StemKind::Bass), &[(0.0, 1.0), (h - 0.01, 1.0), (h, 0.0)]),
                Lane::new(Out, Stem(StemKind::Other), &[(0.0, 1.0), (h, 0.6), (len, 0.0)]),
                Lane::new(Out, Stem(StemKind::Drums), &[(0.0, 1.0), (len * 0.75, 0.8), (len, 0.0)]),
            ],
            TransitionStyle::VocalOver => vec![
                Lane::new(In, Stem(StemKind::Vocals), &[(0.0, 0.0), (len, 0.0)]),
                Lane::new(In, Volume, &[(0.0, 0.0), (q, 1.0)]),
                Lane::new(Out, Stem(StemKind::Drums), &[(0.0, 1.0), (q, 0.0)]),
                Lane::new(Out, Stem(StemKind::Bass), &[(0.0, 1.0), (q - 0.01, 1.0), (q, 0.0)]),
                Lane::new(Out, Stem(StemKind::Other), &[(0.0, 1.0), (q, 0.0)]),
                Lane::new(Out, Stem(StemKind::Vocals), &[(0.0, 1.0), (len * 0.75, 1.0), (len, 0.0)]),
            ],
            TransitionStyle::FilterOut => vec![
                Lane::new(Out, Filter, &[(0.0, 0.5), (len, 0.92)]),
                Lane::new(Out, Volume, &[(0.0, 1.0), (len * 0.75, 0.9), (len, 0.0)]),
                Lane::new(In, Volume, &[(0.0, 0.0), (h, 0.7), (len, 1.0)]),
                Lane::new(In, EqLow, &[(0.0, 0.0), (h - 0.01, 0.0), (h, 0.5)]),
            ],
            TransitionStyle::EchoOut => vec![
                Lane::new(Out, Echo, &[(0.0, 0.0), (len - 4.0_f32.min(len), 0.0), (len - 4.0_f32.min(len) + 0.01, 0.8)]),
                Lane::new(Out, Volume, &[(0.0, 1.0), (len - 4.0_f32.min(len), 1.0), (len, 0.0)]),
                Lane::new(In, Volume, &[(0.0, 0.0), (len - 0.01, 0.0), (len, 1.0)]),
            ],
            TransitionStyle::Cut => vec![
                Lane::new(Out, Volume, &[(0.0, 1.0), (len - 0.01, 1.0), (len, 0.0)]),
                Lane::new(In, Volume, &[(0.0, 0.0), (len - 0.01, 0.0), (len, 1.0)]),
            ],
            TransitionStyle::Custom => TransitionStyle::Crossfade.lanes(len),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub style: TransitionStyle,
    /// Overlap length in beats (multiple of 4; typically 16, 32 or 64).
    pub beats: u32,
    pub lanes: Vec<Lane>,
}

impl Transition {
    pub fn new(style: TransitionStyle, beats: u32) -> Self {
        Transition { style, beats, lanes: style.lanes(beats as f32) }
    }

    /// Value of `param` for `side` at beat `x` of the transition (neutral when there is no lane).
    pub fn value(&self, side: Side, param: Param, x: f32) -> f32 {
        self.lanes
            .iter()
            .find(|l| l.side == side && l.param == param)
            .map(|l| l.value_at(x))
            .unwrap_or(param.neutral())
    }
}

impl Default for Transition {
    fn default() -> Self {
        Transition::new(TransitionStyle::StemBlend, 32)
    }
}

/// One track in a mix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MixTrack {
    pub track_id: TrackId,
    /// Where the track starts playing (s). For every track but the first, this is the point that
    /// is aligned with the outgoing track's `mix_out`.
    pub mix_in: f64,
    /// Where the transition to the next track starts (s).
    pub mix_out: f64,
    /// Semitone shift applied on top of the track's key (harmonic correction).
    pub key_shift: i32,
    pub gain_db: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TempoMode {
    /// Each track plays at its own BPM; the tempo glides to the incoming BPM during the transition.
    Glide,
    /// Every track is stretched to the mix BPM.
    Fixed(f64),
}

/// Mixer settings of one studio lane (lane `i % 2` holds track `i`). They act on top of the
/// transition automation: gain adds to the track gain, EQ/filter are a base setting that
/// transition lanes modulate (EQ gains multiply, a transition filter overrides the lane filter),
/// mute/solo silence lanes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LaneSettings {
    pub gain_db: f32,
    pub mute: bool,
    pub solo: bool,
    /// EQ knobs 0..1 (0.5 flat) low, mid, high.
    pub eq: [f32; 3],
    /// Filter knob 0..1 (0.5 off).
    pub filter: f32,
}

impl Default for LaneSettings {
    fn default() -> Self {
        LaneSettings { gain_db: 0.0, mute: false, solo: false, eq: [0.5; 3], filter: 0.5 }
    }
}

impl LaneSettings {
    pub fn is_neutral(&self) -> bool {
        *self == LaneSettings::default()
    }
}

/// Number of deck lanes in the studio timeline.
pub const MIX_LANES: usize = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mix {
    pub id: i64,
    pub name: String,
    pub tracks: Vec<MixTrack>,
    /// `transitions[i]` joins `tracks[i]` and `tracks[i + 1]`.
    pub transitions: Vec<Transition>,
    pub tempo: TempoMode,
    /// Keep the original key when tempo changes (time stretching) instead of varispeed.
    pub keylock: bool,
    /// Per-lane mixer settings (`lanes[i % MIX_LANES]` applies to track `i`).
    #[serde(default = "default_lanes")]
    pub lanes: Vec<LaneSettings>,
}

fn default_lanes() -> Vec<LaneSettings> {
    vec![LaneSettings::default(); MIX_LANES]
}

impl Default for Mix {
    fn default() -> Self {
        Mix { id: 0, name: "New mix".into(), tracks: vec![], transitions: vec![], tempo: TempoMode::Glide, keylock: true, lanes: default_lanes() }
    }
}

impl Mix {
    /// Keep `transitions.len() == tracks.len() - 1`.
    /// Lane settings for track `i`.
    pub fn lane_of(&self, i: usize) -> LaneSettings {
        self.lanes.get(i % MIX_LANES).copied().unwrap_or_default()
    }

    /// Whether track `i` is audible given mute / solo of the lanes.
    pub fn lane_audible(&self, i: usize) -> bool {
        let any_solo = self.lanes.iter().any(|l| l.solo);
        let l = self.lane_of(i);
        !l.mute && (!any_solo || l.solo)
    }

    pub fn normalize(&mut self) {
        if self.lanes.len() < MIX_LANES {
            self.lanes.resize(MIX_LANES, LaneSettings::default());
        }
        let n = self.tracks.len().saturating_sub(1);
        self.transitions.truncate(n);
        while self.transitions.len() < n {
            self.transitions.push(Transition::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lanes() {
        let t = Transition::new(TransitionStyle::BassSwap, 32);
        assert_eq!(t.value(Side::In, Param::EqLow, 0.0), 0.0);
        assert_eq!(t.value(Side::In, Param::EqLow, 20.0), 0.5);
        assert_eq!(t.value(Side::Out, Param::EqLow, 20.0), 0.0);
        assert_eq!(t.value(Side::Out, Param::EqMid, 3.0), 0.5);
        let l = Lane::new(Side::Out, Param::Volume, &[(0.0, 1.0), (10.0, 0.0)]);
        assert!((l.value_at(5.0) - 0.5).abs() < 1e-6);
    }
}
