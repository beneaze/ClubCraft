//! Real-time preview of a studio mix with seeking, on top of the engine's conductor.
//!
//! Two modes:
//! * **eager** ([`MixPlayer::new`]): every track is loaded up front and stays resident. Simple,
//!   fine for short mixes or when the caller holds the tracks anyway.
//! * **lazy** ([`MixPlayer::lazy`]): only metadata is held. Call [`MixPlayer::poll`] every UI
//!   frame with a loader; it prefetches, on a background thread, the tracks of the residency
//!   window (the current track(s) plus the next, by mix time — see
//!   [`crate::conductor::window_at`]) and hands them to the audio thread with
//!   [`Command::MixTrackReady`]. The audio thread releases tracks that left the window (the
//!   `Arc` goes back to the engine's collector thread), so 2–3 tracks are resident at a time.
//!   After a seek the target tracks are prefetched; until a track arrives its deck plays
//!   silence, then it starts in sync. Nothing is ever loaded on the audio thread.

use crate::command::{Command, DeckId};
use crate::conductor::{window_at, MixSchedule};
use crate::core::EngineCore;
use crate::engine::{CommandSender, EngineHandle};
use crate::state::EngineState;
use crate::track::LoadedTrack;
use anyhow::{anyhow, Result};
use clubcraft_core::{Analysis, LaneSettings, Mix, MixTimeline, TrackId, MIX_LANES};
use parking_lot::Mutex;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Anything that accepts engine commands (the live handle, a sender, or an offline core).
pub trait CommandSink {
    fn submit(&mut self, cmd: Command) -> Result<()>;
}

impl CommandSink for EngineHandle {
    fn submit(&mut self, cmd: Command) -> Result<()> {
        self.send(cmd)
    }
}

impl CommandSink for CommandSender {
    fn submit(&mut self, cmd: Command) -> Result<()> {
        self.send(cmd)
    }
}

impl CommandSink for EngineCore {
    fn submit(&mut self, cmd: Command) -> Result<()> {
        self.apply(cmd);
        Ok(())
    }
}

/// Loads a track's audio + stems (decode, stem cache ...). Called on the prefetch thread.
pub type TrackLoader = Arc<dyn Fn(TrackId) -> Option<Arc<LoadedTrack>> + Send + Sync>;

/// What [`MixPlayer::poll`] did / sees.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrefetchStatus {
    /// Mix tracks that should be resident now.
    pub wanted: Range<usize>,
    /// Mix tracks being loaded on the prefetch thread.
    pub loading: Vec<usize>,
    /// Mix tracks handed to the audio thread during this poll.
    pub delivered: Vec<usize>,
    /// Tracks the loader could not provide (they stay silent).
    pub failed: Vec<TrackId>,
    /// An audible track is still waiting for its audio.
    pub waiting: bool,
}

struct Job {
    id: TrackId,
    loader: TrackLoader,
}

struct Done {
    id: TrackId,
    track: Option<Arc<LoadedTrack>>,
    skipped: bool,
}

struct Prefetcher {
    jobs: crossbeam_channel::Sender<Job>,
    done: crossbeam_channel::Receiver<Done>,
    /// Track ids still wanted (jobs for others are skipped).
    want: Arc<Mutex<Vec<TrackId>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Prefetcher {
    fn new() -> Result<Self> {
        let (jt, jr) = crossbeam_channel::unbounded::<Job>();
        let (dt, dr) = crossbeam_channel::unbounded::<Done>();
        let want: Arc<Mutex<Vec<TrackId>>> = Arc::new(Mutex::new(Vec::new()));
        let w = want.clone();
        let thread = std::thread::Builder::new().name("clubcraft-mix-prefetch".into()).spawn(move || {
            while let Ok(job) = jr.recv() {
                if !w.lock().contains(&job.id) {
                    let _ = dt.send(Done { id: job.id, track: None, skipped: true });
                    continue;
                }
                let track = (job.loader)(job.id);
                if dt.send(Done { id: job.id, track, skipped: false }).is_err() {
                    break;
                }
            }
        })?;
        Ok(Prefetcher { jobs: jt, done: dr, want, thread: Some(thread) })
    }
}

impl Drop for Prefetcher {
    fn drop(&mut self) {
        // closing the job channel ends the thread after its current job; don't block the UI
        let (dummy, _) = crossbeam_channel::unbounded();
        drop(std::mem::replace(&mut self.jobs, dummy));
        drop(self.thread.take());
    }
}

/// Plays a [`Mix`] in real time through two decks; play / pause / seek, live lane edits and
/// structural edits ([`MixPlayer::update`] / [`MixPlayer::update_lazy`]).
pub struct MixPlayer {
    mix: Mix,
    decks: [DeckId; 2],
    timeline: MixTimeline,
    track_ids: Vec<TrackId>,
    analyses: Vec<(TrackId, Analysis)>,
    eager: Option<Vec<Arc<LoadedTrack>>>,
    schedule_id: u64,
    target: Option<(f64, Instant)>,
    prefetch: Option<Prefetcher>,
    in_flight: Vec<TrackId>,
    sent: Vec<(usize, Instant)>,
    failed: Vec<TrackId>,
}

impl MixPlayer {
    /// Eager player: loads every track now (they stay resident while the player lives).
    pub fn new(mix: &Mix, tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>, decks: [DeckId; 2]) -> Result<Self> {
        let s = MixSchedule::new(mix, tracks, decks)?;
        let eager: Vec<Arc<LoadedTrack>> = s.slots.iter().map(|t| t.clone().expect("eager slot")).collect();
        let analyses = s.track_ids.iter().zip(eager.iter()).map(|(id, t)| (*id, t.analysis())).collect();
        Ok(Self::with(mix, decks, s, analyses, Some(eager)))
    }

    /// Lazy player: `analyses` gives each track's metadata (cheap, no audio). Audio is fetched
    /// by [`Self::poll`].
    pub fn lazy(mix: &Mix, analyses: &dyn Fn(TrackId) -> Option<Analysis>, decks: [DeckId; 2]) -> Self {
        let s = MixSchedule::lazy(mix, analyses, decks);
        let an = s.track_ids.iter().filter_map(|id| analyses(*id).map(|a| (*id, a))).collect();
        Self::with(mix, decks, s, an, None)
    }

    fn with(mix: &Mix, decks: [DeckId; 2], s: MixSchedule, analyses: Vec<(TrackId, Analysis)>, eager: Option<Vec<Arc<LoadedTrack>>>) -> Self {
        let mut m = mix.clone();
        m.normalize();
        MixPlayer {
            mix: m,
            decks,
            timeline: s.timeline,
            track_ids: s.track_ids,
            analyses,
            eager,
            schedule_id: 0,
            target: None,
            prefetch: None,
            in_flight: Vec::new(),
            sent: Vec::new(),
            failed: Vec::new(),
        }
    }

    pub fn timeline(&self) -> &MixTimeline {
        &self.timeline
    }

    pub fn mix(&self) -> &Mix {
        &self.mix
    }

    pub fn is_lazy(&self) -> bool {
        self.eager.is_none()
    }

    /// Tracks that should be resident at mix time `t`.
    pub fn wanted(&self, t: f64) -> Range<usize> {
        window_at(&self.timeline, t)
    }

    fn schedule(&self) -> MixSchedule {
        let an = &self.analyses;
        let mut s = MixSchedule::lazy(&self.mix, &|id| an.iter().find(|(x, _)| *x == id).map(|(_, a)| a.clone()), self.decks);
        if let Some(e) = &self.eager {
            s.slots = e.iter().cloned().map(Some).collect();
            s.release_finished = false;
        }
        s
    }

    fn send_schedule(&mut self, sink: &mut dyn CommandSink, from: f64) -> Result<()> {
        let s = self.schedule();
        self.schedule_id = s.id;
        self.sent.clear();
        self.target = Some((from, Instant::now()));
        sink.submit(Command::PlayMix { schedule: Box::new(s), start: from })
    }

    /// Start playing at mix time `from` (s). In lazy mode, keep calling [`Self::poll`].
    pub fn play(&mut self, sink: &mut dyn CommandSink, from: f64) -> Result<()> {
        self.send_schedule(sink, from)
    }

    /// Seek to mix time `t`. In lazy mode the target tracks are prefetched by the next polls;
    /// decks whose audio is missing play silence and join in sync when it arrives.
    pub fn seek(&mut self, sink: &mut dyn CommandSink, t: f64) -> Result<()> {
        self.target = Some((t.max(0.0), Instant::now()));
        sink.submit(Command::MixSeek(t))
    }

    pub fn pause(&self, sink: &mut dyn CommandSink) -> Result<()> {
        sink.submit(Command::MixPause)
    }

    pub fn resume(&self, sink: &mut dyn CommandSink) -> Result<()> {
        sink.submit(Command::MixResume)
    }

    /// Stop; the audio thread hands every resident track back for dropping.
    pub fn stop(&mut self, sink: &mut dyn CommandSink) -> Result<()> {
        self.target = None;
        self.sent.clear();
        sink.submit(Command::MixStop)
    }

    /// Live studio lane mixer edit (gain, mute/solo, EQ, filter). Cheap: no schedule rebuild,
    /// one small allocation-free command; safe to call every frame while a knob is dragged.
    pub fn set_lanes(&mut self, lanes: &[LaneSettings], sink: &mut dyn CommandSink) -> Result<()> {
        let arr: [LaneSettings; MIX_LANES] = std::array::from_fn(|i| lanes.get(i).copied().unwrap_or_default());
        self.mix.lanes = arr.to_vec();
        sink.submit(Command::SetMixLanes(arr))
    }

    fn same_structure(&self, mix: &Mix) -> bool {
        mix.tracks == self.mix.tracks && mix.transitions == self.mix.transitions && mix.tempo == self.mix.tempo && mix.keylock == self.mix.keylock && mix.samples == self.mix.samples && mix.sample_lanes == self.mix.sample_lanes
    }

    fn apply_edit(&mut self, mix: &Mix, sink: &mut dyn CommandSink, state: &EngineState) -> Result<()> {
        let mut m = mix.clone();
        m.normalize();
        self.mix = m;
        let s = self.schedule();
        self.timeline = s.timeline.clone();
        self.track_ids = s.track_ids.clone();
        if state.mix.active && state.mix.playing && state.mix.schedule_id == self.schedule_id {
            // resident tracks are carried over to the new schedule by the audio thread
            self.schedule_id = s.id;
            self.sent.clear();
            self.target = Some((state.mix.time, Instant::now()));
            sink.submit(Command::PlayMix { schedule: Box::new(s), start: state.mix.time })?;
        }
        Ok(())
    }

    /// Replace the mix after an edit (eager player: `tracks` resolves new tracks). When only
    /// the lane settings changed this takes the cheap [`Self::set_lanes`] path.
    pub fn update(&mut self, mix: &Mix, tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>, sink: &mut dyn CommandSink, state: &EngineState) -> Result<()> {
        if self.same_structure(mix) {
            if mix.lanes != self.mix.lanes {
                self.set_lanes(&mix.lanes, sink)?;
            }
            return Ok(());
        }
        if self.eager.is_some() {
            let mut loaded = Vec::with_capacity(mix.tracks.len());
            for mt in &mix.tracks {
                loaded.push(tracks(mt.track_id).ok_or_else(|| anyhow!("track {} not available", mt.track_id))?);
            }
            self.analyses = mix.tracks.iter().zip(loaded.iter()).map(|(m, t)| (m.track_id, t.analysis())).collect();
            self.eager = Some(loaded);
        } else {
            for mt in &mix.tracks {
                if !self.analyses.iter().any(|(id, _)| *id == mt.track_id) {
                    if let Some(t) = tracks(mt.track_id) {
                        self.analyses.push((mt.track_id, t.analysis()));
                    }
                }
            }
        }
        self.apply_edit(mix, sink, state)
    }

    /// Replace the mix after an edit (lazy player: `analyses` gives metadata of new tracks).
    pub fn update_lazy(&mut self, mix: &Mix, analyses: &dyn Fn(TrackId) -> Option<Analysis>, sink: &mut dyn CommandSink, state: &EngineState) -> Result<()> {
        if self.same_structure(mix) {
            if mix.lanes != self.mix.lanes {
                self.set_lanes(&mix.lanes, sink)?;
            }
            return Ok(());
        }
        self.analyses = mix.tracks.iter().filter_map(|m| analyses(m.track_id).map(|a| (m.track_id, a))).collect();
        self.apply_edit(mix, sink, state)
    }

    /// Call every UI frame (lazy mode). Prefetches the residency window on a background thread
    /// and hands finished loads to the audio thread. Cheap when nothing changes.
    pub fn poll(&mut self, sink: &mut dyn CommandSink, state: &EngineState, loader: &TrackLoader) -> Result<PrefetchStatus> {
        let ours = state.mix.active && state.mix.schedule_id == self.schedule_id && self.schedule_id != 0;
        let mut status = PrefetchStatus { waiting: ours && state.mix.waiting, ..Default::default() };
        if self.eager.is_some() || self.schedule_id == 0 {
            return Ok(status);
        }
        // mix time: a pending play/seek target until the audio thread has caught up
        let t = match self.target {
            Some((t, at)) => {
                if (ours && (state.mix.time - t).abs() < 1.0) || at.elapsed() > Duration::from_secs(3) {
                    self.target = None;
                }
                t
            }
            None => state.mix.time,
        };
        let w = window_at(&self.timeline, t);
        status.wanted = w.clone();
        if self.prefetch.is_none() {
            self.prefetch = Some(Prefetcher::new()?);
        }
        let resident = |i: usize| ours && state.mix.is_resident(i);
        let wanted_ids: Vec<TrackId> = w.clone().map(|i| self.track_ids[i]).collect();
        let pf = self.prefetch.as_ref().expect("prefetcher");
        *pf.want.lock() = wanted_ids.clone();
        self.sent.retain(|(i, at)| at.elapsed() < Duration::from_secs(1) && !resident(*i));
        // finished loads → audio thread
        while let Ok(d) = pf.done.try_recv() {
            self.in_flight.retain(|x| *x != d.id);
            match d.track {
                Some(track) => {
                    for i in w.clone() {
                        if self.track_ids[i] == d.id && !resident(i) && !self.sent.iter().any(|(j, _)| *j == i) {
                            sink.submit(Command::MixTrackReady { schedule: self.schedule_id, index: i, track: track.clone() })?;
                            self.sent.push((i, Instant::now()));
                            status.delivered.push(i);
                        }
                    }
                    // not wanted any more (seeked away): dropped here, on the UI thread
                }
                None if !d.skipped => self.failed.push(d.id),
                None => {}
            }
        }
        // new requests (wait until the audio thread runs our schedule, so tracks it carried
        // over from a previous schedule are not loaded twice)
        if ours {
            for i in w.clone() {
                let id = self.track_ids[i];
                if !resident(i) && !self.sent.iter().any(|(j, _)| *j == i) && !self.in_flight.contains(&id) && !self.failed.contains(&id) {
                    if pf.jobs.send(Job { id, loader: loader.clone() }).is_ok() {
                        self.in_flight.push(id);
                    }
                }
            }
        }
        status.loading = w.filter(|i| self.in_flight.contains(&self.track_ids[*i])).collect();
        status.failed = self.failed.clone();
        Ok(status)
    }

    /// Forget load failures (e.g. after the user relocated a file) so they are retried.
    pub fn clear_failures(&mut self) {
        self.failed.clear();
    }

    /// Current mix time from an engine snapshot.
    pub fn position(state: &EngineState) -> f64 {
        state.mix.time
    }
}
