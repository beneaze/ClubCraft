//! Mix timeline: where every track of a [`Mix`] sits in mix time, the tempo at every point and
//! the mapping between mix time and each track's own position.
//!
//! Mix time is primarily measured in **mix beats**. Track `i` covers mix beats
//! `[start_beat, end_beat)`; its beats map 1:1 onto mix beats, which is what keeps overlapping
//! tracks beat-locked. Track `i + 1` starts (at its `mix_in`) on the mix beat where track `i`
//! reaches its `mix_out`; the transition then lasts `transition.beats` beats, after which track
//! `i` stops. Tempo is piecewise linear in mix beats:
//! * [`TempoMode::Glide`]: each track plays at its native BPM; during a transition the tempo
//!   holds the outgoing BPM for the first half and ramps linearly to the incoming BPM over the
//!   second half (DJ.Studio's default "second half" BPM line).
//! * [`TempoMode::Fixed`]: constant tempo.
//!
//! Seconds are obtained by exact integration of `60 / bpm(beat)`.

use crate::analysis::{Analysis, BeatGrid};
use crate::mix::{Mix, Side, TempoMode};
use crate::library::TrackId;

#[derive(Debug, Clone)]
pub struct TimelineTrack {
    pub track_id: TrackId,
    pub grid: BeatGrid,
    /// Whether the grid came from analysis (false = fallback 120 BPM grid).
    pub has_grid: bool,
    pub start_beat: f64,
    pub end_beat: f64,
    /// Track beat index (relative to its grid anchor) at `start_beat`.
    pub track_beat0: f64,
    pub key_shift: i32,
    pub gain_db: f32,
    pub start_sec: f64,
    pub end_sec: f64,
    pub duration: f64,
}

#[derive(Debug, Clone)]
pub struct TimelineTransition {
    /// Index of the outgoing track (`tracks[out]` → `tracks[out + 1]`).
    pub out: usize,
    pub start_beat: f64,
    pub beats: f64,
    pub start_sec: f64,
    pub end_sec: f64,
}

/// Linear tempo segment.
#[derive(Debug, Clone, Copy)]
struct Seg {
    beat: f64,
    beats: f64,
    sec: f64,
    bpm0: f64,
    bpm1: f64,
}

impl Seg {
    fn secs_for(&self, u: f64) -> f64 {
        let (a, c, n) = (self.bpm0, self.bpm1, self.beats.max(1e-12));
        if (c - a).abs() < 1e-9 {
            60.0 * u / a
        } else {
            let k = (c - a) / n;
            60.0 / k * ((a + k * u) / a).ln()
        }
    }
    fn beats_for(&self, s: f64) -> f64 {
        let (a, c, n) = (self.bpm0, self.bpm1, self.beats.max(1e-12));
        if (c - a).abs() < 1e-9 {
            s * a / 60.0
        } else {
            let k = (c - a) / n;
            (a * (s * k / 60.0).exp() - a) / k
        }
    }
    fn bpm_at(&self, u: f64) -> f64 {
        self.bpm0 + (self.bpm1 - self.bpm0) * (u / self.beats.max(1e-12))
    }
}

#[derive(Debug, Clone, Default)]
pub struct MixTimeline {
    pub tracks: Vec<TimelineTrack>,
    pub transitions: Vec<TimelineTransition>,
    pub total_beats: f64,
    pub total_secs: f64,
    segs: Vec<Seg>,
}

/// One track playing at a moment of the mix.
#[derive(Debug, Clone, Copy)]
pub struct ActiveTrack {
    pub index: usize,
    /// Position in the track (s).
    pub track_pos: f64,
    /// Playback rate relative to the track's native tempo.
    pub rate: f64,
    /// If inside a transition: (transition index, side, beats into the transition).
    pub transition: Option<(usize, Side, f32)>,
}

impl MixTimeline {
    pub fn build(mix: &Mix, analysis: &dyn Fn(TrackId) -> Option<Analysis>) -> MixTimeline {
        let mut tl = MixTimeline::default();
        let n = mix.tracks.len();
        if n == 0 {
            return tl;
        }
        let mut beat = 0.0;
        for (i, mt) in mix.tracks.iter().enumerate() {
            let a = analysis(mt.track_id);
            let duration = a.as_ref().map(|a| a.duration).unwrap_or(0.0);
            let (grid, has_grid) = match a.as_ref().and_then(|a| a.grid) {
                Some(g) => (g, true),
                None => (BeatGrid::new(120.0, 0.0), false),
            };
            let b_in = grid.beat_at(mt.mix_in);
            let b_out = grid.beat_at(mt.mix_out.max(mt.mix_in));
            // Unknown length (no analysis yet): assume the track plays through its transition
            // instead of ending where the transition starts.
            let b_end = if duration > 0.0 { grid.beat_at(duration.max(mt.mix_out)) } else { b_out + mix.transitions.get(i).map(|t| t.beats as f64).unwrap_or(64.0) };
            let start_beat = beat;
            let end_beat = if i + 1 < n {
                let tb = mix.transitions.get(i).map(|t| t.beats as f64).unwrap_or(32.0);
                (start_beat + (b_out - b_in) + tb).min(start_beat + (b_end - b_in))
            } else {
                start_beat + (b_end - b_in)
            };
            tl.tracks.push(TimelineTrack {
                track_id: mt.track_id,
                grid,
                has_grid,
                start_beat,
                end_beat,
                track_beat0: b_in,
                key_shift: mt.key_shift,
                gain_db: mt.gain_db,
                start_sec: 0.0,
                end_sec: 0.0,
                duration,
            });
            if i + 1 < n {
                let tb = mix.transitions.get(i).map(|t| t.beats as f64).unwrap_or(32.0);
                let ts = start_beat + (b_out - b_in);
                tl.transitions.push(TimelineTransition { out: i, start_beat: ts, beats: tb, start_sec: 0.0, end_sec: 0.0 });
                beat = ts;
            }
        }
        tl.total_beats = tl.tracks.iter().map(|t| t.end_beat).fold(0.0, f64::max);

        // Tempo segments.
        let native = |i: usize| tl.tracks[i].grid.bpm;
        let mut segs: Vec<Seg> = Vec::new();
        let mut push = |beat: f64, beats: f64, bpm0: f64, bpm1: f64| {
            if beats > 1e-9 {
                segs.push(Seg { beat, beats, sec: 0.0, bpm0, bpm1 });
            }
        };
        match mix.tempo {
            TempoMode::Fixed(b) => push(0.0, tl.total_beats, b, b),
            TempoMode::Glide => {
                let mut cur = 0.0;
                for (k, tr) in tl.transitions.iter().enumerate() {
                    let (a, c) = (native(k), native(k + 1));
                    push(cur, tr.start_beat - cur, a, a);
                    let h = tr.beats / 2.0;
                    push(tr.start_beat, h, a, a);
                    push(tr.start_beat + h, tr.beats - h, a, c);
                    cur = tr.start_beat + tr.beats;
                }
                let last = native(tl.tracks.len() - 1);
                push(cur, tl.total_beats - cur, last, last);
            }
        }
        let mut sec = 0.0;
        for s in segs.iter_mut() {
            s.sec = sec;
            sec += s.secs_for(s.beats);
        }
        tl.total_secs = sec;
        tl.segs = segs;
        for i in 0..tl.tracks.len() {
            tl.tracks[i].start_sec = tl.sec_at_beat(tl.tracks[i].start_beat);
            tl.tracks[i].end_sec = tl.sec_at_beat(tl.tracks[i].end_beat);
        }
        for i in 0..tl.transitions.len() {
            tl.transitions[i].start_sec = tl.sec_at_beat(tl.transitions[i].start_beat);
            tl.transitions[i].end_sec = tl.sec_at_beat(tl.transitions[i].start_beat + tl.transitions[i].beats);
        }
        tl
    }

    fn seg_at_beat(&self, b: f64) -> Option<&Seg> {
        let i = self.segs.partition_point(|s| s.beat <= b);
        self.segs.get(i.saturating_sub(1))
    }

    fn seg_at_sec(&self, t: f64) -> Option<&Seg> {
        let i = self.segs.partition_point(|s| s.sec <= t);
        self.segs.get(i.saturating_sub(1))
    }

    pub fn sec_at_beat(&self, b: f64) -> f64 {
        match self.seg_at_beat(b) {
            Some(s) => s.sec + s.secs_for(b - s.beat),
            None => 0.0,
        }
    }

    pub fn beat_at_sec(&self, t: f64) -> f64 {
        match self.seg_at_sec(t) {
            Some(s) => s.beat + s.beats_for(t - s.sec),
            None => 0.0,
        }
    }

    /// Mix tempo (BPM) at mix beat `b`.
    pub fn bpm_at_beat(&self, b: f64) -> f64 {
        self.seg_at_beat(b).map(|s| s.bpm_at(b - s.beat)).unwrap_or(120.0)
    }

    /// Position in track `i` (s) at mix beat `b`.
    pub fn track_pos(&self, i: usize, b: f64) -> f64 {
        let t = &self.tracks[i];
        t.grid.time_of_beat(t.track_beat0 + (b - t.start_beat))
    }

    /// Mix beat at which track `i` is at position `pos` (s).
    pub fn beat_of_track_pos(&self, i: usize, pos: f64) -> f64 {
        let t = &self.tracks[i];
        t.start_beat + (t.grid.beat_at(pos) - t.track_beat0)
    }

    /// Everything that plays at mix time `t` (s).
    pub fn active_at(&self, t: f64) -> Vec<ActiveTrack> {
        let b = self.beat_at_sec(t);
        let bpm = self.bpm_at_beat(b);
        let mut out = Vec::new();
        for (i, tr) in self.tracks.iter().enumerate() {
            if b < tr.start_beat || b >= tr.end_beat {
                continue;
            }
            let mut transition = None;
            if i > 0 {
                let x = &self.transitions[i - 1];
                if b < x.start_beat + x.beats {
                    transition = Some((i - 1, Side::In, (b - x.start_beat) as f32));
                }
            }
            if let Some(x) = self.transitions.get(i) {
                if b >= x.start_beat {
                    transition = Some((i, Side::Out, (b - x.start_beat) as f32));
                }
            }
            out.push(ActiveTrack { index: i, track_pos: self.track_pos(i, b), rate: bpm / tr.grid.bpm, transition });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mix::*;

    fn an(bpm: f64, dur: f64) -> Analysis {
        Analysis { version: 1, duration: dur, grid: Some(BeatGrid::new(bpm, 0.0)), ..Default::default() }
    }

    #[test]
    fn glide_timeline() {
        let mut mix = Mix::default();
        mix.tracks.push(MixTrack { track_id: 1, mix_in: 0.0, mix_out: 60.0, key_shift: 0, gain_db: 0.0 });
        mix.tracks.push(MixTrack { track_id: 2, mix_in: 0.0, mix_out: 60.0, key_shift: 0, gain_db: 0.0 });
        mix.normalize();
        mix.transitions[0] = Transition::new(TransitionStyle::Crossfade, 32);
        let tl = MixTimeline::build(&mix, &|id| Some(if id == 1 { an(120.0, 120.0) } else { an(126.0, 180.0) }));
        // track 1: mix-out at 60 s = beat 120; transition 32 beats
        assert!((tl.transitions[0].start_beat - 120.0).abs() < 1e-9);
        assert!((tl.transitions[0].start_sec - 60.0).abs() < 1e-9);
        assert!((tl.tracks[0].end_beat - 152.0).abs() < 1e-9);
        // first half of the transition at 120 BPM, then ramp to 126
        assert!((tl.bpm_at_beat(130.0) - 120.0).abs() < 1e-9);
        assert!((tl.bpm_at_beat(144.0) - 123.0).abs() < 1e-9);
        assert!((tl.bpm_at_beat(160.0) - 126.0).abs() < 1e-9);
        // round trip
        for b in [0.0, 50.0, 125.0, 140.0, 151.9, 200.0] {
            assert!((tl.beat_at_sec(tl.sec_at_beat(b)) - b).abs() < 1e-6);
        }
        // during the overlap both tracks are on the same beat phase
        let t = tl.sec_at_beat(140.5);
        let act = tl.active_at(t);
        assert_eq!(act.len(), 2);
        let ph: Vec<f64> = act.iter().map(|a| tl.tracks[a.index].grid.beat_phase(a.track_pos)).collect();
        assert!((ph[0] - ph[1]).abs() < 1e-6, "{ph:?}");
        assert!(matches!(act[0].transition, Some((0, Side::Out, _))));
        assert!(matches!(act[1].transition, Some((0, Side::In, _))));
    }
}
