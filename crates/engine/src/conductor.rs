//! Mix conductor: plays a studio [`Mix`] (or an automix playlist) through two decks.
//!
//! The conductor runs on the audio thread before the decks are processed. Every block it reads
//! the [`MixTimeline`] (from `clubcraft-core`) at the current mix time and, for each track that
//! is audible, makes sure it is loaded on its deck (`tracks[i]` → `decks[i % 2]`), positions it
//! (re-seeking only if it drifted > 3 ms), sets the deck speed so that the deck lands *exactly*
//! on the timeline position at the end of the block (beat-locked, drift-free), and applies the
//! transition lanes (volume, stem gains, EQ, filter, echo) and the studio lane mixer to the deck
//! and its mixer channel. The next track is preloaded and parked on the idle deck as soon as
//! that deck is free.
//!
//! ## Memory: lazily loaded tracks
//! A [`MixSchedule`] holds only per-track metadata (the timeline) plus one *slot*
//! (`Option<Arc<LoadedTrack>>`) per track. Slots can be filled while the mix runs
//! ([`crate::Command::MixTrackReady`]); a track whose slot is still empty plays silence and
//! starts in sync as soon as its audio arrives. With `release_finished` (lazy schedules) the
//! conductor empties every slot outside [`MixSchedule::window_at`] — the current track(s) plus
//! the next one — and hands the `Arc` back for dropping off the audio thread, so only 2–3 tracks
//! are resident at any time.
//!
//! The same machinery serves the offline renderer ([`crate::render`]), the real-time studio
//! preview with seeking ([`crate::MixPlayer`]) and live automix ([`crate::automix`]).

use crate::command::DeckId;
use crate::deck::Deck;
use crate::dsp::eq_combine;
use crate::mixer::{CrossfaderAssign, Mixer};
use crate::state::{MixPlaybackState, MIX_RESIDENT_WORDS};
use crate::track::LoadedTrack;
use anyhow::{anyhow, Result};
use clubcraft_core::{Analysis, LaneSettings, Mix, MixTimeline, Param, Side, StemKind, TrackId, Transition, MIX_LANES};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Tracks kept resident ahead of the first unfinished track (current + next).
pub const MIX_WINDOW: usize = 3;

static NEXT_SCHEDULE_ID: AtomicU64 = AtomicU64::new(1);

/// Everything the audio thread needs to play a mix. Built on the UI thread.
pub struct MixSchedule {
    /// Unique id (process wide); [`crate::Command::MixTrackReady`] must carry it.
    pub id: u64,
    pub timeline: MixTimeline,
    /// Library id of every timeline track.
    pub track_ids: Vec<TrackId>,
    /// Audio + stems per track; `None` = not loaded (plays silence).
    pub slots: Vec<Option<Arc<LoadedTrack>>>,
    /// `transitions[k]` joins track k and k + 1.
    pub transitions: Vec<Transition>,
    pub keylock: bool,
    /// Studio lane mixer settings (`lanes[i % MIX_LANES]` applies to track `i`). Updated live
    /// with [`crate::Command::SetMixLanes`].
    pub lanes: [LaneSettings; MIX_LANES],
    /// Decks used for even / odd tracks.
    pub decks: [DeckId; 2],
    /// Empty (and hand back) slots outside [`Self::window_at`] while playing.
    pub release_finished: bool,
    /// Sample lane clips (audio decoded when the schedule is built).
    pub samples: Vec<crate::samples::SchedSample>,
    /// A sample lane is soloed (silences the decks).
    pub sample_solo: bool,
    /// The mix tracks, for their own stem lanes (`MixTrack::stems`).
    pub mix_tracks: Vec<clubcraft_core::MixTrack>,
}

impl std::fmt::Debug for MixSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MixSchedule")
            .field("id", &self.id)
            .field("tracks", &self.slots.len())
            .field("resident", &self.slots.iter().filter(|s| s.is_some()).count())
            .field("duration", &self.timeline.total_secs)
            .field("decks", &self.decks)
            .finish()
    }
}

impl MixSchedule {
    /// Eager schedule: `tracks` resolves (loads) every `MixTrack::track_id` now and all of them
    /// stay resident. Fine for short mixes or callers that hold the tracks anyway.
    pub fn new(mix: &Mix, tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>, decks: [DeckId; 2]) -> Result<Self> {
        let mut mix = mix.clone();
        mix.normalize();
        let mut loaded = Vec::with_capacity(mix.tracks.len());
        for mt in &mix.tracks {
            loaded.push(tracks(mt.track_id).ok_or_else(|| anyhow!("track {} not available", mt.track_id))?);
        }
        let lookup = |id: TrackId| mix.tracks.iter().position(|m| m.track_id == id).map(|i| loaded[i].analysis());
        let mut s = Self::build(&mix, &lookup, decks);
        s.slots = loaded.into_iter().map(Some).collect();
        s.release_finished = false;
        Ok(s)
    }

    /// Lazy schedule: only metadata from `analyses` (grid, duration, cues — what the timeline
    /// needs); every slot starts empty and the conductor releases finished tracks.
    pub fn lazy(mix: &Mix, analyses: &dyn Fn(TrackId) -> Option<Analysis>, decks: [DeckId; 2]) -> Self {
        let mut mix = mix.clone();
        mix.normalize();
        Self::build(&mix, analyses, decks)
    }

    fn build(mix: &Mix, analyses: &dyn Fn(TrackId) -> Option<Analysis>, decks: [DeckId; 2]) -> Self {
        let timeline = MixTimeline::build(mix, analyses);
        let timeline = &timeline;
        let lanes = std::array::from_fn(|i| mix.lanes.get(i).copied().unwrap_or_default());
        MixSchedule {
            id: NEXT_SCHEDULE_ID.fetch_add(1, Ordering::Relaxed),
            timeline: timeline.clone(),
            track_ids: mix.tracks.iter().map(|t| t.track_id).collect(),
            slots: vec![None; mix.tracks.len()],
            transitions: mix.transitions.clone(),
            keylock: mix.keylock,
            lanes,
            decks,
            release_finished: true,
            samples: mix
                .samples
                .iter()
                .map(|c| crate::samples::SchedSample {
                    lane: c.lane as usize,
                    start: timeline.sec_at_beat(c.start_beat),
                    gain: mix.sample_lane_gain(c.lane as usize) * 10f32.powf(c.gain_db / 20.0),
                    audio: crate::samples::load(&c.path),
                })
                .collect(),
            sample_solo: mix.sample_lanes.iter().any(|l| l.solo),
            mix_tracks: mix.tracks.clone(),
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
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
        let any_solo = self.sample_solo || self.lanes.iter().any(|l| l.solo);
        let l = self.lane_of(i);
        !l.mute && (!any_solo || l.solo)
    }

    /// Mix time (s) at which track `i` is at position `pos` (s).
    pub fn time_of_track_pos(&self, i: usize, pos: f64) -> f64 {
        self.timeline.sec_at_beat(self.timeline.beat_of_track_pos(i, pos))
    }

    /// Tracks that should be resident at mix time `t` (see [`window_at`]).
    pub fn window_at(&self, t: f64) -> Range<usize> {
        window_at(&self.timeline, t)
    }

    /// Bit set of filled slots (first 64 × [`MIX_RESIDENT_WORDS`] tracks).
    pub fn resident_bits(&self) -> [u64; MIX_RESIDENT_WORDS] {
        let mut bits = [0u64; MIX_RESIDENT_WORDS];
        for (i, s) in self.slots.iter().enumerate().take(64 * MIX_RESIDENT_WORDS) {
            if s.is_some() {
                bits[i / 64] |= 1 << (i % 64);
            }
        }
        bits
    }
}

/// The tracks that should be resident at mix time `t`: from the first track that has not
/// finished yet, [`MIX_WINDOW`] tracks (the current one, the incoming one during a transition,
/// and the next). Shared by the conductor (release) and [`crate::MixPlayer`] (prefetch) so the
/// two never disagree.
pub fn window_at(tl: &MixTimeline, t: f64) -> Range<usize> {
    let b = tl.beat_at_sec(t.max(0.0));
    let lo = tl.tracks.iter().position(|tr| tr.end_beat > b).unwrap_or(tl.tracks.len());
    lo..(lo + MIX_WINDOW).min(tl.tracks.len())
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

/// Stem gains of track `i` at mix beat `b` (track position `p` s): from the transition it is
/// in, and the track's own stem lanes for the stems that transition leaves alone.
fn stems_at(s: &MixSchedule, i: usize, b: f64, p: f64) -> [f32; 4] {
    let tl = &s.timeline;
    let mut window: Option<(&Transition, Side, f32)> = None;
    if i > 0 {
        if let Some(x) = tl.transitions.get(i - 1) {
            if b < x.start_beat + x.beats {
                window = Some((&s.transitions[i - 1], Side::In, (b - x.start_beat) as f32));
            }
        }
    }
    if let Some(x) = tl.transitions.get(i) {
        if b >= x.start_beat {
            window = Some((&s.transitions[i], Side::Out, (b - x.start_beat) as f32));
        }
    }
    let mut g = match window {
        Some((t, side, xb)) => StemKind::ALL.map(|st| t.value(side, Param::Stem(st), xb)),
        None => [1.0; 4],
    };
    if let Some(mt) = s.mix_tracks.get(i).filter(|m| !m.stems.is_empty()) {
        for (k, st) in StemKind::ALL.iter().enumerate() {
            let driven = window.is_some_and(|(t, side, _)| t.lanes.iter().any(|l| l.side == side && l.param == Param::Stem(*st)));
            if let (false, Some(v)) = (driven, mt.stem_at(*st, p)) {
                g[k] = v;
            }
        }
    }
    g
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

fn unload_if_holding(deck: &mut Deck, t: &Arc<LoadedTrack>, drop_track: &mut dyn FnMut(Arc<LoadedTrack>)) {
    if deck.track().is_some_and(|x| Arc::ptr_eq(x, t)) {
        if let Some(old) = deck.unload() {
            drop_track(old);
        }
    }
}

impl Conductor {
    /// Install a new schedule. Tracks already resident in the previous schedule are moved over
    /// (matched by track id), so rebuilding after an edit does not reload audio. Returns the
    /// previous schedule (to drop elsewhere).
    pub fn start(
        &mut self,
        mut sched: Box<MixSchedule>,
        start: f64,
        decks: &mut [Deck],
        mixer: &mut Mixer,
        drop_track: &mut dyn FnMut(Arc<LoadedTrack>),
    ) -> Option<Box<MixSchedule>> {
        // keep the decks loaded: tracks carried over below stay on their decks
        let mut old = self.stop_inner(decks, mixer, None);
        if let Some(o) = old.as_mut() {
            for i in 0..sched.slots.len() {
                if sched.slots[i].is_none() {
                    let id = sched.track_ids[i];
                    if let Some(j) = (0..o.slots.len()).find(|&j| o.track_ids[j] == id && o.slots[j].is_some()) {
                        sched.slots[i] = o.slots[j].take();
                    }
                }
            }
        }
        for &d in &sched.decks {
            if let Some(deck) = decks.get_mut(d) {
                // a deck holding a track of the old (lazy) schedule that was not carried over
                if let Some(o) = old.as_ref() {
                    let held = deck.track().cloned();
                    if let Some(t) = held {
                        let in_old = o.slots.iter().flatten().any(|x| Arc::ptr_eq(x, &t));
                        let in_new = sched.slots.iter().flatten().any(|x| Arc::ptr_eq(x, &t));
                        if o.release_finished && in_old && !in_new {
                            if let Some(x) = deck.unload() {
                                drop_track(x);
                            }
                        }
                    }
                }
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

    /// Stop playback, pause the decks and reset automation. For lazy schedules the decks are
    /// unloaded too (their tracks go to `drop_track`), so nothing of the mix stays resident.
    /// Returns the schedule.
    pub fn stop(&mut self, decks: &mut [Deck], mixer: &mut Mixer, drop_track: &mut dyn FnMut(Arc<LoadedTrack>)) -> Option<Box<MixSchedule>> {
        self.stop_inner(decks, mixer, Some(drop_track))
    }

    fn stop_inner(&mut self, decks: &mut [Deck], mixer: &mut Mixer, mut drop_track: Option<&mut dyn FnMut(Arc<LoadedTrack>)>) -> Option<Box<MixSchedule>> {
        let s = self.sched.take()?;
        for &d in &s.decks {
            if let Some(deck) = decks.get_mut(d) {
                if let Some(dt) = drop_track.as_deref_mut() {
                    if s.release_finished && deck.track().is_some_and(|t| s.slots.iter().flatten().any(|x| Arc::ptr_eq(x, t))) {
                        if let Some(old) = deck.unload() {
                            dt(old);
                        }
                    }
                }
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

    /// Fill slot `index` of schedule `schedule`. Returns the track when it was not taken (wrong
    /// schedule, out of range, already filled) so the caller can dispose of it off-thread.
    pub fn set_track(&mut self, schedule: u64, index: usize, track: Arc<LoadedTrack>) -> Option<Arc<LoadedTrack>> {
        match self.sched.as_mut() {
            Some(s) if s.id == schedule && index < s.slots.len() && s.slots[index].is_none() => {
                s.slots[index] = Some(track);
                None
            }
            _ => Some(track),
        }
    }

    pub fn state(&self) -> MixPlaybackState {
        self.state
    }

    /// Mix the sample lanes for the next block (call before [`Self::step`]). Returns whether
    /// anything was written to `out`.
    pub fn render_samples(&self, n: usize, sr: f64, out: &mut [f32]) -> bool {
        match (&self.sched, self.playing) {
            (Some(s), true) if !s.samples.is_empty() => {
                out[..n * 2].fill(0.0);
                crate::samples::render(&s.samples, self.time, n, sr, &mut out[..n * 2])
            }
            _ => false,
        }
    }

    /// Advance by one block of `n` frames at `sr`. Old tracks are passed to `drop_track`.
    pub fn step(&mut self, decks: &mut [Deck], mixer: &mut Mixer, n: usize, sr: f64, drop_track: &mut dyn FnMut(Arc<LoadedTrack>)) {
        let Some(s) = self.sched.as_mut() else {
            self.state = MixPlaybackState { serial: self.serial, finished: self.finished, ..Default::default() };
            return;
        };
        let s: &mut MixSchedule = s;
        let dt = n as f64 / sr;
        let t0 = self.time;
        let t1 = t0 + dt;
        let (b0, b1) = (s.timeline.beat_at_sec(t0), s.timeline.beat_at_sec(t1));
        let mut on_air = 0usize;
        let mut cur_transition = None;
        let mut waiting = false;
        if self.playing && t0 >= s.timeline.total_secs {
            self.playing = false;
            self.finished = true;
            for &d in &s.decks {
                if let Some(deck) = decks.get_mut(d) {
                    deck.pause();
                }
            }
        }
        // release tracks outside the residency window (lazy schedules)
        if s.release_finished {
            let w = window_at(&s.timeline, t0);
            for i in 0..s.slots.len() {
                if w.contains(&i) {
                    continue;
                }
                if let Some(t) = s.slots[i].take() {
                    for &d in &s.decks {
                        if let Some(deck) = decks.get_mut(d) {
                            unload_if_holding(deck, &t, drop_track);
                        }
                    }
                    for st in self.slot_track.iter_mut() {
                        if *st == Some(i) {
                            *st = None;
                        }
                    }
                    drop_track(t);
                }
            }
        }
        if self.playing {
            let tl = &s.timeline;
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
                let Some(track) = s.slots[i].as_ref() else {
                    // audio not here yet: silence on this deck until it arrives
                    waiting = true;
                    if deck.is_playing() {
                        deck.pause();
                    }
                    mixer.channels[d].lane_volume = 0.0;
                    self.slot_track[slot] = None;
                    continue;
                };
                // load if needed
                if !deck.track().is_some_and(|t| Arc::ptr_eq(t, track)) {
                    if let Some(old) = deck.load(track.clone()) {
                        drop_track(old);
                    }
                }
                // a track starting now jumps straight to its automation (no glide from neutral)
                let fresh = self.slot_track[slot] != Some(i);
                if fresh {
                    self.slot_track[slot] = Some(i);
                    deck.set_keylock(s.keylock);
                    deck.set_key_shift(tr.key_shift as f32);
                    let ch = &mut mixer.channels[d];
                    ch.auto_gain_db = track.gain_db.unwrap_or(0.0);
                    ch.fader = 1.0;
                }
                let p0 = tl.track_pos(i, b0);
                let p1 = tl.track_pos(i, b1);
                if p1 <= 0.0 {
                    // before the start of the file (a lead-in): silence until position 0
                    if deck.is_playing() {
                        deck.pause();
                    }
                    if deck.position() != 0.0 {
                        deck.seek(0.0);
                    }
                    continue;
                }
                if !deck.is_playing() || (deck.position() - p0).abs() > 0.003 {
                    deck.seek(p0.max(0.0));
                    deck.play();
                }
                let speed = ((p1 - deck.position()) / dt).clamp(0.05, 4.0);
                deck.set_tempo_override(Some(speed));
                // transition lanes
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
                lanes.stems = stems_at(s, i, b0, p0);
                let mut vol = lanes.volume;
                if deck.has_stems() {
                    // stems are mixed before the time-stretcher, which reads ahead of what is
                    // heard: their gains follow the read position, so they change on time
                    // (at the middle of this block, so a switch lands within half a block)
                    let lead = deck.feed_lead_secs() + (p1 - p0) * 0.5;
                    let stems = if lead > 0.0 {
                        let pf = p0 + lead;
                        stems_at(s, i, tl.beat_of_track_pos(i, pf), pf)
                    } else {
                        lanes.stems
                    };
                    for (k, st) in StemKind::ALL.iter().enumerate() {
                        deck.set_stem_automation(*st, stems[k]);
                    }
                } else if lanes.stems.iter().any(|g| (g - 1.0).abs() > 1e-4) {
                    // no stems: emulate the stem gains with the volume and EQ
                    let (v, eq) = clubcraft_core::stem_emulation(lanes.stems);
                    vol *= v.sqrt();
                    for b in 0..3 {
                        lanes.eq[b] = eq_combine(lanes.eq[b], crate::dsp::eq_gain_to_knob(eq[b]));
                    }
                }
                // studio lane mixer: gain adds to the track gain, mute/solo silence the lane
                // (through the channel's gain smoothers: click free), lane EQ is a base the
                // transition EQ modulates (gains multiply), and a non-neutral transition filter
                // overrides the lane filter.
                let lane = s.lanes[i % MIX_LANES];
                let any_solo = s.sample_solo || s.lanes.iter().any(|l| l.solo);
                if lane.mute || (any_solo && !lane.solo) {
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
                if fresh {
                    ch.snap_next = true;
                    deck.snap_stem_gains();
                }
            }
            // preload the next track on a free deck, parked at its mix-in
            for (i, tr) in tl.tracks.iter().enumerate() {
                if tr.start_beat < b1 {
                    continue;
                }
                let slot = i % 2;
                if self.slot_track[slot].is_none() {
                    let d = s.decks[slot];
                    if let (Some(deck), Some(track)) = (decks.get_mut(d), s.slots[i].as_ref()) {
                        let park = tl.track_pos(i, tr.start_beat).max(0.0);
                        if !deck.track().is_some_and(|t| Arc::ptr_eq(t, track)) {
                            if let Some(old) = deck.load(track.clone()) {
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
            duration: s.timeline.total_secs,
            current_track: on_air,
            transition: cur_transition,
            bpm: s.timeline.bpm_at_beat(b0),
            finished: self.finished,
            serial: self.serial,
            schedule_id: s.id,
            resident: s.resident_bits(),
            waiting,
        };
    }
}
