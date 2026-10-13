//! Mix conductor: plays a studio [`Mix`] (or an automix playlist) through two decks.
//!
//! The conductor runs on the audio thread before the decks are processed. Every block it reads
//! the [`MixTimeline`] (from `clubcraft-core`) at the current mix time and, for each track that
//! is audible, makes sure it is loaded on its deck (`tracks[i]` → `decks[i % 2]`), positions it
//! (re-seeking only if it drifted > 3 ms), sets the deck speed so that the deck lands *exactly*
//! on the timeline position at the end of the block (beat-locked, drift-free), and applies the
//! transition lanes (volume, stem gains, EQ, filter, echo) to the deck and its mixer channel.
//! The next track is preloaded and parked on the idle deck as soon as that deck is free.
//!
//! The same machinery serves the offline renderer ([`crate::render`]), the real-time studio
//! preview with seeking ([`crate::MixPlayer`]) and live automix ([`crate::automix`]).

use crate::command::DeckId;
use crate::deck::Deck;
use crate::mixer::{CrossfaderAssign, Mixer};
use crate::state::MixPlaybackState;
use crate::track::LoadedTrack;
use anyhow::{anyhow, Result};
use crate::dsp::eq_combine;
use clubcraft_core::{LaneSettings, Mix, MixTimeline, Param, Side, StemKind, TrackId, Transition, MIX_LANES};
use std::sync::Arc;

/// Everything the audio thread needs to play a mix. Built on the UI thread.
pub struct MixSchedule {
    pub timeline: MixTimeline,
    /// One loaded track per timeline track.
    pub tracks: Vec<Arc<LoadedTrack>>,
    /// `transitions[k]` joins track k and k + 1.
    pub transitions: Vec<Transition>,
    pub keylock: bool,
    /// Studio lane mixer settings (`lanes[i % MIX_LANES]` applies to track `i`). Updated live
    /// with [`crate::Command::SetMixLanes`].
    pub lanes: [LaneSettings; MIX_LANES],
    /// Decks used for even / odd tracks.
    pub decks: [DeckId; 2],
}

impl std::fmt::Debug for MixSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixSchedule").field("tracks", &self.tracks.len()).field("duration", &self.timeline.total_secs).field("decks", &self.decks).finish()
    }
}

impl MixSchedule {
    /// Build from a studio mix. `tracks` resolves every `MixTrack::track_id`.
    pub fn new(mix: &Mix, tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>, decks: [DeckId; 2]) -> Result<Self> {
        let mut mix = mix.clone();
        mix.normalize();
        let mut loaded = Vec::with_capacity(mix.tracks.len());
        for mt in &mix.tracks {
            loaded.push(tracks(mt.track_id).ok_or_else(|| anyhow!("track {} not available", mt.track_id))?);
        }
        let lookup = |id: TrackId| {
            mix.tracks.iter().position(|m| m.track_id == id).map(|i| loaded[i].analysis())
        };
        let timeline = MixTimeline::build(&mix, &lookup);
        let lanes = std::array::from_fn(|i| mix.lanes.get(i).copied().unwrap_or_default());
        Ok(MixSchedule { timeline, tracks: loaded, transitions: mix.transitions.clone(), keylock: mix.keylock, lanes, decks })
    }

    pub fn duration(&self) -> f64 {
        self.timeline.total_secs
    }

    /// Lane settings of track `i`.
    pub fn lane_of(&self, i: usize) -> LaneSettings {
        self.lanes[i % MIX_LANES]
    }

    /// Track `i` is audible given the lanes' mute / solo (same rule as [`Mix::lane_audible`]).
    pub fn lane_audible(&self, i: usize) -> bool {
        let any_solo = self.lanes.iter().any(|l| l.solo);
        let l = self.lane_of(i);
        !l.mute && (!any_solo || l.solo)
    }

    /// Mix time (s) at which track `i` is at position `pos` (s).
    pub fn time_of_track_pos(&self, i: usize, pos: f64) -> f64 {
        self.timeline.sec_at_beat(self.timeline.beat_of_track_pos(i, pos))
    }
}

/// Lane values applied to one side.
struct LaneValues {
    volume: f32,
    stems: [f32; 4],
    eq: [f32; 3],
    filter: f32,
    echo: f32,
}

impl LaneValues {
    fn neutral() -> Self {
        LaneValues { volume: 1.0, stems: [1.0; 4], eq: [0.5; 3], filter: 0.5, echo: 0.0 }
    }

    fn from_transition(t: &Transition, side: Side, x: f32) -> Self {
        LaneValues {
            volume: t.value(side, Param::Volume, x),
            stems: StemKind::ALL.map(|s| t.value(side, Param::Stem(s), x)),
            eq: [t.value(side, Param::EqLow, x), t.value(side, Param::EqMid, x), t.value(side, Param::EqHigh, x)],
            filter: t.value(side, Param::Filter, x),
            echo: t.value(side, Param::Echo, x),
        }
    }
}

/// Audio-thread mix player state.
#[derive(Default)]
pub(crate) struct Conductor {
    pub sched: Option<Box<MixSchedule>>,
    pub time: f64,
    pub playing: bool,
    pub finished: bool,
    pub serial: u64,
    slot_track: [Option<usize>; 2],
    state: MixPlaybackState,
}

impl Conductor {
    /// Install a new schedule. Returns the previous one (to drop elsewhere).
    pub fn start(&mut self, sched: Box<MixSchedule>, start: f64, decks: &mut [Deck], mixer: &mut Mixer) -> Option<Box<MixSchedule>> {
        let old = self.stop(decks, mixer);
        for &d in &sched.decks {
            if let Some(deck) = decks.get_mut(d) {
                deck.set_keylock(sched.keylock);
                deck.pause();
            }
            if let Some(ch) = mixer.channels.get_mut(d) {
                ch.reset_automation();
                ch.fader = 1.0;
                ch.assign = CrossfaderAssign::Thru;
            }
        }
        self.time = start.max(0.0);
        self.playing = true;
        self.finished = false;
        self.serial += 1;
        self.slot_track = [None; 2];
        self.sched = Some(sched);
        old
    }

    /// Stop playback, pause the decks and reset automation. Returns the schedule.
    pub fn stop(&mut self, decks: &mut [Deck], mixer: &mut Mixer) -> Option<Box<MixSchedule>> {
        let s = self.sched.take()?;
        for &d in &s.decks {
            if let Some(deck) = decks.get_mut(d) {
                deck.pause();
                deck.set_tempo_override(None);
                for st in StemKind::ALL {
                    deck.set_stem_automation(st, 1.0);
                }
            }
            if let Some(ch) = mixer.channels.get_mut(d) {
                ch.reset_automation();
            }
        }
        self.playing = false;
        self.slot_track = [None; 2];
        Some(s)
    }

    pub fn pause(&mut self, decks: &mut [Deck]) {
        self.playing = false;
        if let Some(s) = &self.sched {
            for &d in &s.decks {
                if let Some(deck) = decks.get_mut(d) {
                    deck.pause();
                }
            }
        }
    }

    /// Replace the lane settings of the playing schedule (allocation free; takes effect on the
    /// next block, smoothed by the channel's parameter smoothers).
    pub fn set_lanes(&mut self, lanes: [LaneSettings; MIX_LANES]) {
        if let Some(s) = self.sched.as_mut() {
            s.lanes = lanes;
        }
    }

    pub fn state(&self) -> MixPlaybackState {
        self.state
    }

    /// Advance by one block of `n` frames at `sr`. Old tracks are passed to `drop_track`.
    pub fn step(&mut self, decks: &mut [Deck], mixer: &mut Mixer, n: usize, sr: f64, drop_track: &mut dyn FnMut(Arc<LoadedTrack>)) {
        let Some(s) = self.sched.as_ref() else {
            self.state = MixPlaybackState { serial: self.serial, finished: self.finished, ..Default::default() };
            return;
        };
        let tl = &s.timeline;
        let dt = n as f64 / sr;
        let t0 = self.time;
        let t1 = t0 + dt;
        let b0 = tl.beat_at_sec(t0);
        let b1 = tl.beat_at_sec(t1);
        let mut on_air = 0usize;
        let mut cur_transition = None;
        if self.playing {
            if t0 >= tl.total_secs {
                self.playing = false;
                self.finished = true;
                for &d in &s.decks {
                    if let Some(deck) = decks.get_mut(d) {
                        deck.pause();
                    }
                }
            }
        }
        if self.playing {
            for (i, tr) in tl.tracks.iter().enumerate() {
                let slot = i % 2;
                let d = s.decks[slot];
                let (Some(deck), true) = (decks.get_mut(d), d < mixer.channels.len()) else { continue };
                let active = b1 > tr.start_beat && b0 < tr.end_beat;
                if !active {
                    if self.slot_track[slot] == Some(i) && (b0 >= tr.end_beat || b1 <= tr.start_beat) {
                        // finished (or seeked away): stop and free the deck
                        deck.pause();
                        deck.set_tempo_override(None);
                        mixer.channels[d].reset_automation();
                        self.slot_track[slot] = None;
                    }
                    continue;
                }
                // load if needed
                if !deck.track().is_some_and(|t| Arc::ptr_eq(t, &s.tracks[i])) {
                    if let Some(old) = deck.load(s.tracks[i].clone()) {
                        drop_track(old);
                    }
                }
                if self.slot_track[slot] != Some(i) {
                    self.slot_track[slot] = Some(i);
                    deck.set_keylock(s.keylock);
                    deck.set_key_shift(tr.key_shift as f32);
                    let ch = &mut mixer.channels[d];
                    ch.auto_gain_db = s.tracks[i].gain_db.unwrap_or(0.0);
                    ch.fader = 1.0;
                }
                let p0 = tl.track_pos(i, b0);
                let p1 = tl.track_pos(i, b1);
                if !deck.is_playing() || (deck.position() - p0).abs() > 0.003 {
                    deck.seek(p0.max(0.0));
                    deck.play();
                }
                let speed = ((p1 - deck.position()) / dt).clamp(0.05, 4.0);
                deck.set_tempo_override(Some(speed));
                // lanes
                let mut lanes = LaneValues::neutral();
                if i > 0 {
                    let x = &tl.transitions[i - 1];
                    if b0 < x.start_beat + x.beats {
                        let xb = (b0 - x.start_beat) as f32;
                        lanes = LaneValues::from_transition(&s.transitions[i - 1], Side::In, xb);
                        cur_transition = Some((i - 1, xb));
                        if xb >= x.beats as f32 * 0.5 {
                            on_air = on_air.max(i);
                        }
                    } else {
                        on_air = on_air.max(i);
                    }
                } else {
                    on_air = on_air.max(i);
                }
                if let Some(x) = tl.transitions.get(i) {
                    if b0 >= x.start_beat {
                        let xb = (b0 - x.start_beat) as f32;
                        lanes = LaneValues::from_transition(&s.transitions[i], Side::Out, xb);
                        cur_transition = Some((i, xb));
                    }
                }
                let mut vol = lanes.volume;
                if deck.has_stems() {
                    for (k, st) in StemKind::ALL.iter().enumerate() {
                        deck.set_stem_automation(*st, lanes.stems[k]);
                    }
                } else {
                    // no stems: approximate the stem lanes with the channel volume
                    vol *= lanes.stems.iter().sum::<f32>() / 4.0;
                }
                // studio lane mixer: gain adds to the track gain, mute/solo silence the lane
                // (through the channel's gain smoothers: click free), lane EQ is a base the
                // transition EQ modulates (gains multiply), and a non-neutral transition filter
                // overrides the lane filter.
                let lane = s.lane_of(i);
                if !s.lane_audible(i) {
                    vol = 0.0;
                }
                let ch = &mut mixer.channels[d];
                ch.trim_db = tr.gain_db + lane.gain_db;
                ch.lane_volume = vol;
                for b in 0..3 {
                    ch.eq_knobs[b] = eq_combine(lane.eq[b], lanes.eq[b]);
                }
                ch.filter_knob = if (lanes.filter - Param::Filter.neutral()).abs() > 1e-4 { lanes.filter } else { lane.filter };
                ch.echo_send = lanes.echo;
            }
            // preload the next track on a free deck, parked at its mix-in
            for (i, tr) in tl.tracks.iter().enumerate() {
                if tr.start_beat < b1 {
                    continue;
                }
                let slot = i % 2;
                if self.slot_track[slot].is_none() {
                    let d = s.decks[slot];
                    if let Some(deck) = decks.get_mut(d) {
                        let park = tl.track_pos(i, tr.start_beat).max(0.0);
                        if !deck.track().is_some_and(|t| Arc::ptr_eq(t, &s.tracks[i])) {
                            if let Some(old) = deck.load(s.tracks[i].clone()) {
                                drop_track(old);
                            }
                        }
                        if deck.is_playing() {
                            deck.pause();
                        }
                        if (deck.position() - park).abs() > 1e-6 {
                            deck.seek(park);
                        }
                        deck.set_keylock(s.keylock);
                        deck.set_key_shift(tr.key_shift as f32);
                        mixer.channels[d].lane_volume = 0.0;
                    }
                }
                break;
            }
            self.time = t1;
        }
        self.state = MixPlaybackState {
            active: true,
            playing: self.playing,
            time: self.time,
            duration: tl.total_secs,
            current_track: on_air,
            transition: cur_transition,
            bpm: tl.bpm_at_beat(b0),
            finished: self.finished,
            serial: self.serial,
        };
    }
}
