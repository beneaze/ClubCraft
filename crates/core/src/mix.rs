//! Studio mixes: an ordered list of tracks joined by beat-matched transitions, each with
//! automation lanes (volume, per-stem gains, EQ, filter and effects). The same model drives the
//! offline renderer, the studio preview and the live-deck automix.

use crate::analysis::{BeatGrid, StemKind};
use crate::library::TrackId;
use std::collections::BTreeMap;
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
    /// Reverb send 0..1, taken before the fader: the track can dissolve into its reverb (or
    /// emerge from it) while its volume moves.
    Reverb,
    /// White-noise sweep 0..1 over the transition: level and band-pass frequency rise with the
    /// value (riser, downsweep, swoosh).
    Noise,
    /// Beat loop: 0 = off, above it the length of the loop the track repeats, from one bar
    /// (0.2) halving to 1/4 beat (1.0). Captured on the track's beat grid.
    Loop,
    /// Vinyl brake 0..1: the track slows down (pitch with it) and stops at 1.
    Brake,
    /// Side-chain pump 0..1: the track ducks on every beat of its grid, by this depth.
    Pump,
}

impl Param {
    pub fn neutral(self) -> f32 {
        match self {
            Param::Volume | Param::Stem(_) => 1.0,
            Param::EqLow | Param::EqMid | Param::EqHigh | Param::Filter => 0.5,
            Param::Echo | Param::Reverb | Param::Noise | Param::Loop | Param::Brake | Param::Pump => 0.0,
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
            Param::Reverb => "Reverb".into(),
            Param::Noise => "Noise".into(),
            Param::Loop => "Loop".into(),
            Param::Brake => "Brake".into(),
            Param::Pump => "Side-chain".into(),
        }
    }
}

/// Automation curve over a transition. `x` is the position in beats from the transition start
/// (0..=length_beats), `y` the parameter value. Straight or bent lines between points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    pub side: Side,
    pub param: Param,
    pub points: Vec<(f32, f32)>,
    /// Shape of the segment that starts at each point, -1..=1: 0 straight, above 0 bowed up,
    /// below 0 bowed down. Missing entries are straight, so lanes without bends (and mixes saved
    /// before bends existed) are plain lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bends: Vec<f32>,
}

/// Steepness of a fully bent segment (see [`bend_shape`]).
const BEND_K: f32 = 5.0;

impl Lane {
    pub fn new(side: Side, param: Param, points: &[(f32, f32)]) -> Self {
        Lane { side, param, points: points.to_vec(), bends: Vec::new() }
    }

    pub fn value_at(&self, x: f32) -> f32 {
        interpolate_bent(&self.points, &self.bends, x, self.param.neutral())
    }

    /// Bend of the segment from point `i` to `i + 1`.
    pub fn bend(&self, i: usize) -> f32 {
        self.bends.get(i).copied().unwrap_or(0.0)
    }

    pub fn set_bend(&mut self, i: usize, b: f32) {
        if self.bends.len() <= i {
            if b == 0.0 {
                return;
            }
            self.bends.resize(i + 1, 0.0);
        }
        self.bends[i] = b.clamp(-1.0, 1.0);
    }

    /// Insert point `pt` at index `k`; both halves of a split segment keep its bend.
    pub fn insert_point(&mut self, k: usize, pt: (f32, f32)) {
        let k = k.min(self.points.len());
        self.points.insert(k, pt);
        if !self.bends.is_empty() {
            let b = if k > 0 { self.bend(k - 1) } else { 0.0 };
            self.bends.resize(self.bends.len().max(k), 0.0);
            self.bends.insert(k, b);
        }
    }

    /// Remove point `k`; the joined segment keeps the bend of the one on its left.
    pub fn remove_point(&mut self, k: usize) {
        if k < self.points.len() {
            self.points.remove(k);
            if k < self.bends.len() {
                self.bends.remove(k);
            }
        }
    }

    /// Keep the points `f` accepts (with their bends).
    pub fn retain_points(&mut self, mut f: impl FnMut(&(f32, f32)) -> bool) {
        let bends = std::mem::take(&mut self.bends);
        let mut kept = Vec::new();
        let mut i = 0;
        self.points.retain(|p| {
            let keep = f(p);
            if let Some(b) = bends.get(i).filter(|_| keep) {
                kept.push(*b);
            }
            i += 1;
            keep
        });
        self.bends = kept;
    }

    /// Set the curve to `v` from `x0` to `x1`, with hard switches at both ends (a stroke of the
    /// pencil). The curve outside keeps its values and shape.
    pub fn paint(&mut self, x0: f32, x1: f32, v: f32) {
        const EPS: f32 = 1e-4;
        let (x0, x1) = (x0.min(x1), x0.max(x1));
        // the value just left of x0 and just right of x1 (a step there counts on its own side)
        let left = self.points.iter().find(|p| (p.0 - x0).abs() < EPS).map_or_else(|| self.value_at(x0), |p| p.1);
        let right = self.points.iter().rev().find(|p| (p.0 - x1).abs() < EPS).map_or_else(|| self.value_at(x1), |p| p.1);
        let (has_left, has_right) = (self.points.iter().any(|p| p.0 < x0 - EPS), self.points.iter().any(|p| p.0 > x1 + EPS));
        let mut out: Vec<(f32, f32, f32)> = Vec::with_capacity(self.points.len() + 4);
        for (i, p) in self.points.iter().enumerate().filter(|(_, p)| p.0 < x0 - EPS) {
            out.push((p.0, p.1, self.bend(i)));
        }
        if has_left {
            out.push((x0, left, 0.0));
        }
        out.extend([(x0, v, 0.0), (x1, v, 0.0)]);
        if has_right {
            out.push((x1, right, 0.0));
        }
        for (i, p) in self.points.iter().enumerate().filter(|(_, p)| p.0 > x1 + EPS) {
            out.push((p.0, p.1, self.bend(i)));
        }
        self.points = out.iter().map(|p| (p.0, p.1)).collect();
        self.bends = out.iter().map(|p| p.2).collect();
        self.tidy();
    }

    /// Drop points that change nothing: repeats, the middle of three points at one time, and the
    /// middle of a flat run.
    pub fn tidy(&mut self) {
        const EPS: f32 = 1e-4;
        let mut i = 1;
        while i + 1 < self.points.len() {
            let (a, b, c) = (self.points[i - 1], self.points[i], self.points[i + 1]);
            let same_time = (a.0 - b.0).abs() < EPS && (b.0 - c.0).abs() < EPS;
            let flat = (a.1 - b.1).abs() < EPS && (b.1 - c.1).abs() < EPS && self.bend(i - 1) == 0.0 && self.bend(i) == 0.0;
            let repeat = (a.0 - b.0).abs() < EPS && (a.1 - b.1).abs() < EPS;
            if same_time || flat || repeat {
                self.remove_point(i);
            } else {
                i += 1;
            }
        }
        if self.points.len() == 2 && (self.points[0].0 - self.points[1].0).abs() < EPS && (self.points[0].1 - self.points[1].1).abs() < EPS {
            self.remove_point(1);
        }
        while self.bends.last() == Some(&0.0) {
            self.bends.pop();
        }
    }
}

/// Where a segment bent by `bend` is at `t` (0..1 of its length), as 0..1 of its rise.
/// `rising`: the segment goes up. Bowing up means fast first on a rise, slow first on a fall.
pub fn bend_shape(t: f32, bend: f32, rising: bool) -> f32 {
    let k = if rising { -bend } else { bend } * BEND_K;
    if k.abs() < 1e-3 {
        return t;
    }
    ((k * t).exp() - 1.0) / (k.exp() - 1.0)
}

/// The bend that makes a segment from `a` to `b` pass through `v` at its middle.
pub fn bend_through(a: f32, b: f32, v: f32) -> f32 {
    if (b - a).abs() < 1e-4 {
        return 0.0;
    }
    // at the middle, bend_shape = 1 / (e^(k/2) + 1)
    let r = ((v - a) / (b - a)).clamp(0.005, 0.995);
    let k = 2.0 * (1.0 / r - 1.0).ln();
    let bend = k / BEND_K;
    (if b > a { -bend } else { bend }).clamp(-1.0, 1.0)
}

/// Piecewise-linear value of `p` at `x` (`neutral` without points; held before the first and after
/// the last point; two points at the same `x` make a step).
fn interpolate(p: &[(f32, f32)], x: f32, neutral: f32) -> f32 {
    interpolate_bent(p, &[], x, neutral)
}

/// [`interpolate`] with the segments shaped by `bends` (see [`Lane::bends`]).
fn interpolate_bent(p: &[(f32, f32)], bends: &[f32], x: f32, neutral: f32) -> f32 {
    match p.len() {
        0 => neutral,
        1 => p[0].1,
        _ => {
            if x <= p[0].0 {
                return p[0].1;
            }
            for (i, w) in p.windows(2).enumerate() {
                let (a, b) = (w[0], w[1]);
                if x <= b.0 {
                    let t = if b.0 > a.0 { (x - a.0) / (b.0 - a.0) } else { 1.0 };
                    let bend = bends.get(i).copied().unwrap_or(0.0);
                    let t = if bend == 0.0 { t } else { bend_shape(t, bend, b.1 > a.1) };
                    return a.1 + (b.1 - a.1) * t;
                }
            }
            p[p.len() - 1].1
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
    /// Mashup: the outgoing vocals sing over the incoming instrumental for the whole transition
    /// (instrumentals swap within the first bar, the incoming vocals return at the end).
    Mashup,
    /// Lanes edited by hand.
    Custom,
}

impl TransitionStyle {
    pub const ALL: [TransitionStyle; 9] = [
        TransitionStyle::StemBlend,
        TransitionStyle::BassSwap,
        TransitionStyle::Crossfade,
        TransitionStyle::VocalOver,
        TransitionStyle::FilterOut,
        TransitionStyle::EchoOut,
        TransitionStyle::Cut,
        TransitionStyle::Mashup,
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
            TransitionStyle::Mashup => "Mashup",
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
            TransitionStyle::Mashup => {
                let bar = 4.0f32.min(len * 0.25);
                vec![
                    // outgoing: instrumental out over the first bar, vocals until the last bar
                    Lane::new(Out, Stem(StemKind::Drums), &[(0.0, 1.0), (bar, 0.0)]),
                    Lane::new(Out, Stem(StemKind::Bass), &[(0.0, 1.0), (bar * 0.5 - 0.01, 1.0), (bar * 0.5, 0.0)]),
                    Lane::new(Out, Stem(StemKind::Other), &[(0.0, 1.0), (bar, 0.0)]),
                    Lane::new(Out, Stem(StemKind::Vocals), &[(0.0, 1.0), (len - bar, 1.0), (len, 0.0)]),
                    // incoming: instrumental in over the first bar, own vocals muted until the end
                    Lane::new(In, Stem(StemKind::Drums), &[(0.0, 0.0), (bar, 1.0)]),
                    Lane::new(In, Stem(StemKind::Bass), &[(0.0, 0.0), (bar * 0.5 - 0.01, 0.0), (bar * 0.5, 1.0)]),
                    Lane::new(In, Stem(StemKind::Other), &[(0.0, 0.0), (bar, 1.0)]),
                    Lane::new(In, Stem(StemKind::Vocals), &[(0.0, 0.0), (len - 1.0, 0.0), (len, 1.0)]),
                ]
            }
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
    /// Locked: kept as it is (mix points, length, curves) by re-ordering and automatic mix
    /// points, and not editable until unlocked.
    #[serde(default)]
    pub locked: bool,
    /// Mix tempo during the transition (BPM, [`TempoMode::Glide`] only): held at `[0]` over the
    /// first half and ramping linearly (in beats) to `[1]` over the second half. `None` (the
    /// default) = the outgoing track's tempo, ramping to the incoming track's tempo. Imported
    /// DJ.Studio projects set it to DJ.Studio's tempo-lane values, which run slightly faster than
    /// the beat grids (see `docs/interop/djstudio.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tempo: Option<[f64; 2]>,
}

impl Transition {
    pub fn new(style: TransitionStyle, beats: u32) -> Self {
        Transition { style, beats, lanes: style.lanes(beats as f32), locked: false, tempo: None }
    }

    /// Change the length to `beats`, keeping the shape of every curve (points scale with the
    /// window).
    pub fn set_beats(&mut self, beats: u32) {
        let (from, to) = (self.beats.max(1) as f32, beats.max(1) as f32);
        for l in &mut self.lanes {
            for p in &mut l.points {
                p.0 = p.0 / from * to;
            }
        }
        self.beats = beats.max(1);
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

impl Transition {
    /// Whether the transition drives stems (it then needs stems on both tracks).
    pub fn uses_stems(&self) -> bool {
        self.lanes.iter().any(|l| matches!(l.param, Param::Stem(_)))
    }

    /// What plays while a track of this transition has no stems yet: each side's stem curves are
    /// emulated with its volume and EQ. Every band (low, mid, high) gets the level of the stems
    /// that play in it (energy-weighted: the bass stem drives the lows, drums the highs, melody and
    /// vocals the mids); the loudest band sets the volume and the EQ cuts the others. A drum
    /// slope, a bass swap or a vocal drop thus keeps its own timing and shape. The transition's
    /// own volume and EQ lanes (crossfade, bass swap) still apply on top.
    pub fn without_stems(&self) -> Transition {
        self.without_stems_on(true, true)
    }

    /// [`Self::without_stems`] for the outgoing and/or the incoming side only (the other side
    /// keeps its stem lanes).
    pub fn without_stems_on(&self, out: bool, inn: bool) -> Transition {
        use Param::*;
        if !self.uses_stems() {
            return self.clone();
        }
        let len = self.beats as f32;
        let emulated = |side: Side| if side == Side::Out { out } else { inn };
        let mut lanes: Vec<Lane> = self.lanes.iter().filter(|l| !(matches!(l.param, Stem(_)) && emulated(l.side))).cloned().collect();
        for side in [Side::Out, Side::In].into_iter().filter(|&sd| emulated(sd)) {
            let stems: Vec<&Lane> = self.lanes.iter().filter(|l| l.side == side && matches!(l.param, Stem(_))).collect();
            if stems.is_empty() {
                continue;
            }
            let params = [Volume, EqLow, EqMid, EqHigh];
            let own = |p: Param| self.lanes.iter().find(|l| l.side == side && l.param == p);
            // sample every breakpoint (both sides of a step) and points in between on ramps
            let mut keys: Vec<f32> = vec![0.0, len];
            for l in stems.iter().copied().chain(params.iter().filter_map(|&p| own(p))) {
                for &(x, _) in &l.points {
                    keys.extend([x - 1e-3, x, x + 1e-3]);
                }
            }
            keys.retain(|x| (0.0..=len).contains(x));
            keys.sort_by(|a, b| a.total_cmp(b));
            keys.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
            let mut xs = Vec::with_capacity(keys.len() * 8);
            for w in keys.windows(2) {
                for i in 0..8 {
                    xs.push(w[0] + (w[1] - w[0]) * i as f32 / 8.0);
                }
            }
            xs.push(len);
            let stem = |k: usize, x: f32| stems.iter().find(|l| l.param == Stem(StemKind::ALL[k])).map_or(1.0, |l| l.value_at(x)).clamp(0.0, 1.0);
            let value = |p: Param, x: f32| own(p).map_or(p.neutral(), |l| l.value_at(x));
            let mut out: [Vec<(f32, f32)>; 4] = Default::default();
            for &x in &xs {
                let (v, g) = stem_emulation([stem(0, x), stem(1, x), stem(2, x), stem(3, x)]);
                // volume gain = value², EQ gain = (2·knob)² below flat
                out[0].push((x, value(Volume, x) * v.sqrt()));
                for b in 0..3 {
                    let k = value(params[b + 1], x);
                    out[b + 1].push((x, eq_knob(eq_gain(k) * g[b])));
                }
            }
            for (p, pts) in params.into_iter().zip(out) {
                let pts = simplify(&pts, 2e-3);
                lanes.retain(|l| !(l.side == side && l.param == p));
                if pts.iter().any(|q| (q.1 - p.neutral()).abs() > 1e-3) {
                    lanes.push(Lane::new(side, p, &pts));
                }
            }
        }
        Transition { style: self.style, beats: self.beats, lanes, locked: self.locked, tempo: self.tempo }
    }
}

/// How stem gains (drums, bass, melody, vocals; 0..1) sound on a track without stems: returns the
/// volume gain and the low, mid and high EQ gains (≤ 1). Each band gets the energy-weighted level
/// of the stems that play in it; the loudest band sets the volume and the EQ cuts the others.
pub fn stem_emulation(s: [f32; 4]) -> (f32, [f32; 3]) {
    let band = |w: [f32; 4]| (0..4).map(|k| w[k] * s[k].clamp(0.0, 1.0).powi(2)).sum::<f32>().sqrt();
    let g = [band(STEM_BAND_LOW), band(STEM_BAND_MID), band(STEM_BAND_HIGH)];
    let v = g.iter().copied().fold(0.0, f32::max);
    if v <= 1e-6 {
        return (0.0, [1.0; 3]);
    }
    (v, g.map(|b| b / v))
}

/// Share of each stem (drums, bass, melody, vocals) in the low, mid and high EQ band.
const STEM_BAND_LOW: [f32; 4] = [0.25, 0.7, 0.05, 0.0];
const STEM_BAND_MID: [f32; 4] = [0.2, 0.1, 0.4, 0.3];
const STEM_BAND_HIGH: [f32; 4] = [0.5, 0.0, 0.25, 0.25];

/// EQ knob (0.5 flat) → gain, `(2k)²` below flat, +12 dB per knob unit above.
fn eq_gain(k: f32) -> f32 {
    if k <= 0.5 {
        (2.0 * k).powi(2)
    } else {
        10f32.powf(12.0 * (k - 0.5) / 20.0)
    }
}

fn eq_knob(g: f32) -> f32 {
    if g <= 0.0 {
        0.0
    } else if g <= 1.0 {
        0.5 * g.sqrt()
    } else {
        0.5 + 20.0 * g.log10() / 12.0
    }
}

/// Drop the points that lie on the line between the points kept around them (within `tol`).
fn simplify(p: &[(f32, f32)], tol: f32) -> Vec<(f32, f32)> {
    if p.len() <= 2 {
        return p.to_vec();
    }
    let on_line = |a: (f32, f32), c: (f32, f32), mid: &[(f32, f32)]| {
        mid.iter().all(|b| {
            let t = if c.0 > a.0 { (b.0 - a.0) / (c.0 - a.0) } else { 0.0 };
            (a.1 + (c.1 - a.1) * t - b.1).abs() <= tol
        })
    };
    let mut out = vec![p[0]];
    let mut a = 0;
    for i in 1..p.len() - 1 {
        if !on_line(p[a], p[i + 1], &p[a + 1..=i]) {
            out.push(p[i]);
            a = i;
        }
    }
    out.push(p[p.len() - 1]);
    out
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
    /// Stem automation of the track itself (e.g. stems muted in part of a song). It plays outside
    /// the transitions; inside one, the transition's own lane for that stem wins.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stems: Vec<StemLane>,
}

impl MixTrack {
    pub fn new(track_id: TrackId, mix_in: f64, mix_out: f64) -> Self {
        MixTrack { track_id, mix_in, mix_out, key_shift: 0, gain_db: 0.0, stems: Vec::new() }
    }

    /// Set `stem` to `gain` between track times `t0` and `t1` (s), with 10 ms ramps, keeping the
    /// lane outside the range. A lane that ends up at full level everywhere is removed.
    pub fn set_stem_range(&mut self, stem: StemKind, t0: f64, t1: f64, gain: f32) {
        const FADE: f32 = 0.01;
        let (t0, t1) = (t0.min(t1) as f32, t0.max(t1) as f32);
        let idx = match self.stems.iter().position(|l| l.stem == stem) {
            Some(i) => i,
            None => {
                self.stems.push(StemLane { stem, points: Vec::new() });
                self.stems.len() - 1
            }
        };
        let pts = &mut self.stems[idx].points;
        let (before, after) = (interpolate(pts, t0 - FADE, 1.0), interpolate(pts, t1 + FADE, 1.0));
        pts.retain(|p| p.0 < t0 - FADE || p.0 > t1 + FADE);
        pts.extend([(t0 - FADE, before), (t0, gain), (t1, gain), (t1 + FADE, after)]);
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        if pts.iter().all(|p| (p.1 - 1.0).abs() < 1e-4) {
            self.stems.remove(idx);
        }
    }

    /// Gain of `stem` from the track's own stem lanes at track time `t` (s), if it has one.
    pub fn stem_at(&self, stem: StemKind, t: f64) -> Option<f32> {
        self.stems.iter().find(|l| l.stem == stem).map(|l| interpolate(&l.points, t as f32, 1.0))
    }
}

/// Stem lane of a whole track: points (track seconds, gain 0..1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StemLane {
    pub stem: StemKind,
    pub points: Vec<(f32, f32)>,
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
/// Number of sample lanes (one-shots, loops, FX) under the deck lanes.
pub const SAMPLE_LANES: usize = 2;

/// An audio file placed on a sample lane. It plays at its own speed from `start_beat` (mix
/// beats), on top of the decks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleClip {
    pub lane: u8,
    pub path: std::path::PathBuf,
    pub start_beat: f64,
    #[serde(default)]
    pub gain_db: f32,
}

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
    /// Clips on the sample lanes.
    #[serde(default)]
    pub samples: Vec<SampleClip>,
    /// Mixer settings of the sample lanes (gain, mute, solo).
    #[serde(default = "default_sample_lanes")]
    pub sample_lanes: Vec<LaneSettings>,
    /// Beat grids that tracks follow in this mix instead of their analysed grid, by track id.
    /// Imported projects keep the other application's grid so that mix points and transitions
    /// land on the same beats there and here. Empty for ClubCraft's own mixes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub grids: BTreeMap<TrackId, BeatGrid>,
}

fn default_sample_lanes() -> Vec<LaneSettings> {
    vec![LaneSettings::default(); SAMPLE_LANES]
}

fn default_lanes() -> Vec<LaneSettings> {
    vec![LaneSettings::default(); MIX_LANES]
}

impl Default for Mix {
    fn default() -> Self {
        Mix { id: 0, name: "New mix".into(), tracks: vec![], transitions: vec![], tempo: TempoMode::Glide, keylock: true, lanes: default_lanes(), samples: vec![], sample_lanes: default_sample_lanes(), grids: BTreeMap::new() }
    }
}

impl Mix {
    /// Keep `transitions.len() == tracks.len() - 1`.
    /// Lane settings for track `i`.
    pub fn lane_of(&self, i: usize) -> LaneSettings {
        self.lanes.get(i % MIX_LANES).copied().unwrap_or_default()
    }

    /// Linear gain of sample lane `l` given its fader, mute and solo (solo on any lane, deck or
    /// sample, silences the others).
    pub fn sample_lane_gain(&self, l: usize) -> f32 {
        let s = self.sample_lanes.get(l).copied().unwrap_or_default();
        let any_solo = self.lanes.iter().chain(&self.sample_lanes).any(|x| x.solo);
        if s.mute || (any_solo && !s.solo) {
            0.0
        } else {
            10f32.powf(s.gain_db / 20.0)
        }
    }

    /// Whether track `i` is audible given mute / solo of the lanes.
    pub fn lane_audible(&self, i: usize) -> bool {
        let any_solo = self.lanes.iter().chain(&self.sample_lanes).any(|l| l.solo);
        let l = self.lane_of(i);
        !l.mute && (!any_solo || l.solo)
    }

    pub fn normalize(&mut self) {
        if self.lanes.len() < MIX_LANES {
            self.lanes.resize(MIX_LANES, LaneSettings::default());
        }
        if self.sample_lanes.len() < SAMPLE_LANES {
            self.sample_lanes.resize(SAMPLE_LANES, LaneSettings::default());
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

    #[test]
    fn bent_segments_pass_through_their_handle_and_keep_their_ends() {
        for (a, b) in [(0.0, 1.0), (1.0, 0.0), (0.2, 0.7)] {
            for v in [0.15, 0.5, 0.8] {
                let v = a + (b - a) * v;
                let mut l = Lane::new(Side::Out, Param::Volume, &[(0.0, a), (8.0, b)]);
                l.set_bend(0, bend_through(a, b, v));
                assert!((l.value_at(4.0) - v).abs() < 1e-3, "{a}->{b} through {v}: {}", l.value_at(4.0));
                assert!((l.value_at(0.0) - a).abs() < 1e-6 && (l.value_at(8.0) - b).abs() < 1e-6);
            }
        }
        // bowed up is above the straight line, rising or falling
        let mut up = Lane::new(Side::Out, Param::Volume, &[(0.0, 0.0), (8.0, 1.0), (16.0, 0.0)]);
        up.set_bend(0, 0.5);
        up.set_bend(1, 0.5);
        assert!(up.value_at(4.0) > 0.5 && up.value_at(12.0) > 0.5);
        // mixes saved before bends load as straight lines
        let old: Lane = serde_json::from_str(r#"{"side":"Out","param":"Volume","points":[[0.0,1.0],[4.0,0.0]]}"#).unwrap();
        assert!(old.bends.is_empty() && (old.value_at(2.0) - 0.5).abs() < 1e-6);
        assert!(!serde_json::to_string(&old).unwrap().contains("bends"));
    }

    #[test]
    fn points_keep_their_bends_through_edits() {
        let mut l = Lane::new(Side::Out, Param::Volume, &[(0.0, 0.0), (4.0, 1.0), (8.0, 0.0)]);
        l.set_bend(1, -0.5);
        l.insert_point(2, (6.0, 0.4));
        assert_eq!(l.bends, vec![0.0, -0.5, -0.5]);
        l.remove_point(0);
        assert_eq!((l.points.len(), l.bends.clone()), (3, vec![-0.5, -0.5]));
        l.retain_points(|p| p.0 != 6.0);
        assert_eq!((l.points, l.bends), (vec![(4.0, 1.0), (8.0, 0.0)], vec![-0.5]));
    }

    #[test]
    fn painting_makes_hard_switches_and_merges_runs() {
        let mut l = Lane::new(Side::Out, Param::Stem(StemKind::Vocals), &[(0.0, 1.0), (16.0, 1.0)]);
        // off over bars 2 and 3, one beat cell at a time
        for b in 4..12 {
            l.paint(b as f32, b as f32 + 1.0, 0.0);
        }
        assert_eq!(l.points, vec![(0.0, 1.0), (4.0, 1.0), (4.0, 0.0), (12.0, 0.0), (12.0, 1.0), (16.0, 1.0)]);
        // painting at the start needs no switch before it
        l.paint(0.0, 2.0, 0.5);
        assert_eq!(&l.points[..3], &[(0.0, 0.5), (2.0, 0.5), (2.0, 1.0)]);
        // painting over a switch at its own value removes it
        l.paint(2.0, 12.0, 1.0);
        assert_eq!(l.points, vec![(0.0, 0.5), (2.0, 0.5), (2.0, 1.0), (16.0, 1.0)]);
        // a ramp next to the stroke keeps its shape
        let mut r = Lane::new(Side::Out, Param::Volume, &[(0.0, 0.0), (8.0, 1.0), (16.0, 1.0)]);
        r.set_bend(0, 0.4);
        let before = r.value_at(3.0);
        r.paint(12.0, 13.0, 0.0);
        assert!((r.value_at(3.0) - before).abs() < 1e-6);
        assert_eq!(r.value_at(12.5), 0.0);
    }
}

#[cfg(test)]
mod serde_tests {
    use super::*;

    #[test]
    fn grids_and_transition_tempo_are_optional() {
        let mut mix = Mix::default();
        mix.tracks.push(MixTrack::new(1, 0.0, 60.0));
        mix.tracks.push(MixTrack::new(2, 0.0, 60.0));
        mix.normalize();
        // ClubCraft's own mixes do not write the new fields ...
        let json = serde_json::to_string(&mix).unwrap();
        assert!(!json.contains("grids") && !json.contains("tempo\":["), "{json}");
        // ... and mixes saved before they existed still load
        let back: Mix = serde_json::from_str(&json).unwrap();
        assert!(back.grids.is_empty() && back.transitions[0].tempo.is_none());
        // imported mixes keep them
        mix.grids.insert(2, crate::analysis::BeatGrid::new(124.0, 0.1));
        mix.transitions[0].tempo = Some([124.2, 126.3]);
        let back: Mix = serde_json::from_str(&serde_json::to_string(&mix).unwrap()).unwrap();
        assert_eq!(back.grids[&2].bpm, 124.0);
        assert_eq!(back.transitions[0].tempo, Some([124.2, 126.3]));
    }
}

#[cfg(test)]
mod stemless_tests {
    use super::*;

    #[test]
    fn stem_ranges_mute_and_restore_parts_of_a_track() {
        let mut t = MixTrack::new(1, 0.0, 200.0);
        t.set_stem_range(StemKind::Vocals, 30.0, 60.0, 0.0);
        assert_eq!(t.stem_at(StemKind::Vocals, 20.0), Some(1.0));
        assert_eq!(t.stem_at(StemKind::Vocals, 45.0), Some(0.0));
        assert_eq!(t.stem_at(StemKind::Vocals, 70.0), Some(1.0));
        // a second range keeps the first one
        t.set_stem_range(StemKind::Vocals, 100.0, 110.0, 0.0);
        assert_eq!(t.stem_at(StemKind::Vocals, 45.0), Some(0.0));
        assert_eq!(t.stem_at(StemKind::Vocals, 105.0), Some(0.0));
        // switching both back on removes the lane
        t.set_stem_range(StemKind::Vocals, 30.0, 60.0, 1.0);
        t.set_stem_range(StemKind::Vocals, 100.0, 110.0, 1.0);
        assert!(t.stems.is_empty());
    }

    #[test]
    fn stem_blend_without_stems_follows_the_stems() {
        let t = Transition::new(TransitionStyle::StemBlend, 32);
        assert!(t.uses_stems());
        let f = t.without_stems();
        assert!(!f.uses_stems());
        // outgoing: mostly up while its drums play, gone at the end; lows cut when the bass leaves
        assert!(f.value(Side::Out, Param::Volume, 4.0) > 0.9);
        assert!(f.value(Side::Out, Param::Volume, 20.0) > 0.75);
        assert!(f.value(Side::Out, Param::Volume, 32.0) < 1e-3);
        assert!(f.value(Side::Out, Param::EqLow, 15.0) > f.value(Side::Out, Param::EqLow, 17.0) + 0.05);
        // incoming: silent at the start, full at the end
        assert!(f.value(Side::In, Param::Volume, 0.0) < 1e-3);
        assert!((f.value(Side::In, Param::Volume, 32.0) - 1.0).abs() < 1e-3);
        assert!((f.value(Side::In, Param::EqLow, 32.0) - 0.5).abs() < 1e-3);
        // styles without stems are unchanged
        let b = Transition::new(TransitionStyle::BassSwap, 32);
        assert_eq!(b.without_stems(), b);
    }

    #[test]
    fn each_stem_transition_keeps_its_own_shape() {
        use StemKind::*;
        let side = |d: &[(f32, f32)], rest: f32| {
            let mut t = Transition { style: TransitionStyle::StemBlend, beats: 32, lanes: vec![], locked: false, tempo: None };
            t.lanes.push(Lane::new(Side::Out, Param::Stem(Drums), d));
            for k in [Bass, Other, Vocals] {
                t.lanes.push(Lane::new(Side::Out, Param::Stem(k), &[(0.0, rest)]));
            }
            t.without_stems()
        };
        // drums sloping out over the window, the rest muted: the level follows the drums
        let a = side(&[(0.0, 1.0), (32.0, 0.0)], 0.0);
        let (v0, v16, v32) = (a.value(Side::Out, Param::Volume, 0.0), a.value(Side::Out, Param::Volume, 16.0), a.value(Side::Out, Param::Volume, 32.0));
        assert!(v0 > v16 && v16 > v32 && v32 < 1e-3, "{v0} {v16} {v32}");
        assert!(v0 < 0.95, "only the drums play: quieter than the full track");
        // drums held, the rest playing: full level until the end
        let b = side(&[(0.0, 1.0)], 1.0);
        assert!((b.value(Side::Out, Param::Volume, 16.0) - 1.0).abs() < 1e-3);
        // a crossfade lane still applies on top
        let mut c = Transition::new(TransitionStyle::StemBlend, 32);
        c.lanes.retain(|l| l.side == Side::In);
        c.lanes.push(Lane::new(Side::Out, Param::Volume, &[(0.0, 1.0), (16.0, 1.0), (32.0, 0.0)]));
        c.lanes.push(Lane::new(Side::Out, Param::Stem(Vocals), &[(0.0, 1.0)]));
        let c = c.without_stems();
        assert!((c.value(Side::Out, Param::Volume, 8.0) - 1.0).abs() < 1e-3);
        assert!((c.value(Side::Out, Param::Volume, 24.0) - 0.5).abs() < 1e-2);
    }
}

// ------------------------------------------------------------------ simple per-parameter modes

/// How one stem, EQ band, the volume or an effect behaves across a transition: the simple
/// choices of the transition editor (like DJ.Studio's None / Swap / Crossfade / Full).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Stem: silent on both sides during the transition. EQ / effect: no change.
    Off,
    /// Hard switch at the midpoint.
    Swap,
    /// Gradual handover (stems between 40 % and 60 %, EQ between 25 % and 75 %, volume across
    /// the whole transition).
    Crossfade,
    /// Both tracks keep it for the whole transition.
    Full,
    /// Volume: the outgoing track fades over the second half, the incoming comes up over the first.
    Half,
    /// Filter: high-pass sweep out of the outgoing track.
    SweepOut,
    /// Filter: low-pass sweep into the incoming track.
    SweepIn,
    /// Filter: the outgoing track closes down with a low-pass.
    FilterDown,
    /// Filter: high-pass out of the outgoing and low-pass into the incoming track at once.
    SweepBoth,
    /// Echo: the outgoing track echoes out over the last bar.
    EchoOut,
    /// Echo: one beat of the outgoing track thrown into the echo just before the swap.
    EchoThrow,
    /// Echo: the echo send on the outgoing track builds up over the second half.
    EchoBuild,
    /// Reverb: the outgoing track dissolves into reverb over the last two bars.
    ReverbOut,
    /// Reverb: the incoming track emerges from reverb over the first half.
    ReverbIn,
    /// Reverb: both, the old track washes out while the new one emerges.
    Wash,
    /// Noise: a rising noise sweep over the second half, cut at the end.
    Riser,
    /// Noise: a falling noise sweep from the swap to the end.
    Downsweep,
    /// Noise: a short swell of noise around the swap.
    Swoosh,
    /// Loop: the outgoing track repeats one bar from the swap on.
    LoopOut,
    /// Loop: the outgoing track rolls over the last two bars (one bar, then shorter and shorter).
    RollOut,
    /// Loop: the incoming track repeats one bar until the swap.
    LoopIn,
    /// Brake: the outgoing track stops like a turntable over the last bar.
    BrakeOut,
    /// Brake: the outgoing track brakes over the half bar before the swap and stays stopped.
    BrakeSwap,
    /// Side-chain: the outgoing track pumps on every beat, deeper and deeper.
    PumpOut,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Off => "Off",
            Mode::Swap => "Swap",
            Mode::Crossfade => "Crossfade",
            Mode::Full => "Full",
            Mode::Half => "Second half",
            Mode::SweepOut => "High-pass out",
            Mode::SweepIn => "Low-pass in",
            Mode::FilterDown => "Low-pass out",
            Mode::SweepBoth => "Both",
            Mode::EchoOut => "Echo out",
            Mode::EchoThrow => "Echo throw",
            Mode::EchoBuild => "Echo build",
            Mode::ReverbOut => "Reverb out",
            Mode::ReverbIn => "Reverb in",
            Mode::Wash => "Wash",
            Mode::Riser => "Riser",
            Mode::Downsweep => "Downsweep",
            Mode::Swoosh => "Swoosh",
            Mode::LoopOut => "Loop out",
            Mode::RollOut => "Roll out",
            Mode::LoopIn => "Loop in",
            Mode::BrakeOut => "Brake out",
            Mode::BrakeSwap => "Brake at swap",
            Mode::PumpOut => "Pump out",
        }
    }

    /// Modes offered for a parameter.
    pub fn for_param(p: Param) -> &'static [Mode] {
        match p {
            Param::Stem(_) => &[Mode::Off, Mode::Swap, Mode::Crossfade, Mode::Full],
            Param::EqLow | Param::EqMid | Param::EqHigh => &[Mode::Off, Mode::Swap, Mode::Crossfade],
            Param::Volume => &[Mode::Full, Mode::Crossfade, Mode::Half, Mode::Swap],
            Param::Filter => &[Mode::Off, Mode::SweepOut, Mode::SweepIn, Mode::FilterDown, Mode::SweepBoth],
            Param::Echo => &[Mode::Off, Mode::EchoOut, Mode::EchoThrow, Mode::EchoBuild],
            Param::Reverb => &[Mode::Off, Mode::ReverbOut, Mode::ReverbIn, Mode::Wash],
            Param::Noise => &[Mode::Off, Mode::Riser, Mode::Downsweep, Mode::Swoosh],
            Param::Loop => &[Mode::Off, Mode::LoopOut, Mode::RollOut, Mode::LoopIn],
            Param::Brake => &[Mode::Off, Mode::BrakeOut, Mode::BrakeSwap],
            Param::Pump => &[Mode::Off, Mode::PumpOut, Mode::Crossfade, Mode::Swap],
        }
    }
}

/// Lanes (both sides) for `mode` of `param` over `len` beats.
pub fn mode_lanes(param: Param, mode: Mode, len: f32) -> Vec<Lane> {
    use Side::*;
    let h = len * 0.5;
    let e = 0.01f32.min(len * 0.01);
    let bar = 4.0f32.min(len * 0.25);
    let pair = |out: &[(f32, f32)], inc: &[(f32, f32)]| vec![Lane::new(Out, param, out), Lane::new(In, param, inc)];
    match (param, mode) {
        (Param::Stem(_), Mode::Off) => pair(&[(0.0, 0.0), (len, 0.0)], &[(0.0, 0.0), (len - e, 0.0), (len, 1.0)]),
        (Param::Stem(_), Mode::Swap) | (Param::Volume, Mode::Swap) => pair(&[(0.0, 1.0), (h - e, 1.0), (h, 0.0)], &[(0.0, 0.0), (h - e, 0.0), (h, 1.0)]),
        (Param::Stem(_), Mode::Crossfade) => pair(&[(0.0, 1.0), (len * 0.4, 1.0), (len * 0.6, 0.0)], &[(0.0, 0.0), (len * 0.4, 0.0), (len * 0.6, 1.0)]),
        (Param::EqLow | Param::EqMid | Param::EqHigh, Mode::Swap) => pair(&[(0.0, 0.5), (h - e, 0.5), (h, 0.0)], &[(0.0, 0.0), (h - e, 0.0), (h, 0.5)]),
        (Param::EqLow | Param::EqMid | Param::EqHigh, Mode::Crossfade) => pair(&[(0.0, 0.5), (len * 0.25, 0.5), (len * 0.75, 0.0)], &[(0.0, 0.0), (len * 0.25, 0.0), (len * 0.75, 0.5)]),
        (Param::Volume, Mode::Crossfade) => pair(&[(0.0, 1.0), (len, 0.0)], &[(0.0, 0.0), (len, 1.0)]),
        (Param::Volume, Mode::Half) => pair(&[(0.0, 1.0), (h, 1.0), (len, 0.0)], &[(0.0, 0.0), (h, 1.0)]),
        (Param::Filter, Mode::SweepOut) => vec![Lane::new(Out, param, &[(0.0, 0.5), (len, 0.92)])],
        (Param::Filter, Mode::SweepIn) => vec![Lane::new(In, param, &[(0.0, 0.08), (len, 0.5)])],
        (Param::Filter, Mode::FilterDown) => vec![Lane::new(Out, param, &[(0.0, 0.5), (len, 0.08)])],
        (Param::Filter, Mode::SweepBoth) => pair(&[(0.0, 0.5), (len, 0.92)], &[(0.0, 0.08), (len, 0.5)]),
        (Param::Echo, Mode::EchoOut) => {
            let s = len - 4.0f32.min(len);
            vec![Lane::new(Out, param, &[(0.0, 0.0), (s, 0.0), (s + e, 0.8)])]
        }
        (Param::Echo, Mode::EchoThrow) => {
            let s = (h - 1.0).max(0.0);
            vec![Lane::new(Out, param, &[(0.0, 0.0), (s - e, 0.0), (s, 0.9), (h, 0.9), (h + e, 0.0)])]
        }
        (Param::Echo, Mode::EchoBuild) => vec![Lane::new(Out, param, &[(0.0, 0.0), (h, 0.0), (len, 0.9)])],
        (Param::Reverb, Mode::ReverbOut) => vec![Lane::new(Out, param, &[(0.0, 0.0), (len - 2.0 * bar, 0.0), (len, 1.0)])],
        (Param::Reverb, Mode::ReverbIn) => vec![Lane::new(In, param, &[(0.0, 1.0), (h, 0.0)])],
        (Param::Reverb, Mode::Wash) => pair(&[(0.0, 0.0), (h, 0.0), (len, 1.0)], &[(0.0, 1.0), (h, 0.0)]),
        // noise rides on the outgoing channel: it is one sound for the whole transition
        (Param::Noise, Mode::Riser) => vec![Lane::new(Out, param, &[(0.0, 0.0), (h, 0.0), (len - e, 1.0), (len, 0.0)])],
        (Param::Noise, Mode::Downsweep) => vec![Lane::new(Out, param, &[(0.0, 0.0), (h - e, 0.0), (h, 1.0), (len, 0.0)])],
        (Param::Noise, Mode::Swoosh) => vec![Lane::new(Out, param, &[(0.0, 0.0), (h - bar, 0.0), (h, 1.0), (h + bar, 0.0)])],
        (Param::Loop, Mode::LoopOut) => vec![Lane::new(Out, param, &[(0.0, 0.0), (h - e, 0.0), (h, loop_value(4.0))])],
        (Param::Loop, Mode::RollOut) => {
            // one bar, then two beats, one beat and half a beat, each over half the time before
            let r = len - 2.0 * bar;
            let mut pts = vec![(0.0, 0.0)];
            let mut prev = 0.0;
            for (at, beats) in [(r, 4.0), (r + bar, 2.0), (r + 1.5 * bar, 1.0), (r + 1.75 * bar, 0.5)] {
                pts.extend([(at - e, prev), (at, loop_value(beats))]);
                prev = loop_value(beats);
            }
            vec![Lane::new(Out, param, &pts)]
        }
        (Param::Loop, Mode::LoopIn) => vec![Lane::new(In, param, &[(0.0, loop_value(4.0)), (h - e, loop_value(4.0)), (h, 0.0)])],
        (Param::Brake, Mode::BrakeOut) => vec![Lane::new(Out, param, &[(0.0, 0.0), (len - bar, 0.0), (len, 1.0)])],
        (Param::Brake, Mode::BrakeSwap) => vec![Lane::new(Out, param, &[(0.0, 0.0), (h - bar * 0.5, 0.0), (h, 1.0)])],
        (Param::Pump, Mode::PumpOut) => vec![Lane::new(Out, param, &[(0.0, 0.0), (len, 0.85)])],
        (Param::Pump, Mode::Crossfade) => pair(&[(0.0, 0.0), (len, 0.85)], &[(0.0, 0.85), (len, 0.0)]),
        (Param::Pump, Mode::Swap) => pair(&[(0.0, 0.0), (h - e, 0.0), (h, 0.85)], &[(0.0, 0.85), (h - e, 0.85), (h, 0.0)]),
        _ => vec![], // Off / Full: neutral
    }
}

/// Loop lane value for a loop of `beats` (4, 2, 1, 1/2 or 1/4 beats).
pub fn loop_value(beats: f32) -> f32 {
    0.2 + 0.2 * (4.0 / beats.max(0.25)).log2().round().clamp(0.0, 4.0)
}

/// Loop length (beats) a [`Param::Loop`] value stands for, `None` when the loop is off.
pub fn loop_beats(v: f32) -> Option<f32> {
    if v < 0.1 {
        return None;
    }
    let steps = ((v - 0.2) / 0.2).round().clamp(0.0, 4.0);
    Some(4.0 / 2f32.powf(steps))
}

impl Transition {
    /// The simple mode `param` currently follows, `None` for a hand-drawn / preset-specific curve.
    pub fn mode(&self, param: Param) -> Option<Mode> {
        let len = self.beats as f32;
        let probe = |lanes: &[Lane], side: Side, x: f32| lanes.iter().find(|l| l.side == side && l.param == param).map(|l| l.value_at(x)).unwrap_or(param.neutral());
        Mode::for_param(param).iter().copied().find(|&m| {
            let want = mode_lanes(param, m, len);
            (0..=64).all(|i| {
                let x = len * i as f32 / 64.0;
                [Side::Out, Side::In].iter().all(|&s| (probe(&self.lanes, s, x) - probe(&want, s, x)).abs() < 0.03)
            })
        })
    }

    /// Make `param` follow `mode` on both sides; the transition becomes a custom one.
    pub fn set_mode(&mut self, param: Param, mode: Mode) {
        self.lanes.retain(|l| l.param != param);
        self.lanes.extend(mode_lanes(param, mode, self.beats as f32));
        self.style = TransitionStyle::Custom;
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    #[test]
    fn modes_roundtrip_and_presets_are_recognised() {
        let mut t = Transition::new(TransitionStyle::BassSwap, 32);
        assert_eq!(t.mode(Param::EqLow), Some(Mode::Swap));
        assert_eq!(t.mode(Param::Volume), None); // bass swap's own volume curve
        assert_eq!(t.mode(Param::Stem(StemKind::Vocals)), Some(Mode::Full));
        for k in StemKind::ALL {
            for &m in Mode::for_param(Param::Stem(k)) {
                t.set_mode(Param::Stem(k), m);
                assert_eq!(t.mode(Param::Stem(k)), Some(m));
            }
        }
        assert_eq!(t.style, TransitionStyle::Custom);
        t.set_mode(Param::Volume, Mode::Half);
        assert_eq!(t.mode(Param::Volume), Some(Mode::Half));
        // every effect mode is told apart from the others, at every length
        for beats in [16, 32, 64] {
            let mut t = Transition::new(TransitionStyle::Crossfade, beats);
            for p in [Param::Filter, Param::Echo, Param::Reverb, Param::Noise, Param::Loop, Param::Brake, Param::Pump] {
                assert_eq!(t.mode(p), Some(Mode::Off), "{p:?}");
                for &m in Mode::for_param(p) {
                    t.set_mode(p, m);
                    assert_eq!(t.mode(p), Some(m), "{p:?} {m:?} over {beats} beats");
                }
            }
        }
    }

    #[test]
    fn loop_values_name_their_lengths() {
        assert_eq!(loop_beats(0.0), None);
        for beats in [4.0, 2.0, 1.0, 0.5, 0.25] {
            assert_eq!(loop_beats(loop_value(beats)), Some(beats));
        }
        // a roll out ends on the shortest step before the end and is off before it starts
        let mut t = Transition::new(TransitionStyle::Crossfade, 32);
        t.set_mode(Param::Loop, Mode::RollOut);
        assert_eq!(loop_beats(t.value(Side::Out, Param::Loop, 10.0)), None);
        assert_eq!(loop_beats(t.value(Side::Out, Param::Loop, 25.0)), Some(4.0));
        assert_eq!(loop_beats(t.value(Side::Out, Param::Loop, 31.9)), Some(0.5));
    }
}
