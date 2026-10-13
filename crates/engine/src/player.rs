//! Real-time preview of a studio mix with seeking, on top of the engine's conductor.

use crate::command::{Command, DeckId};
use crate::conductor::MixSchedule;
use crate::core::EngineCore;
use crate::engine::{CommandSender, EngineHandle};
use crate::state::EngineState;
use crate::track::LoadedTrack;
use anyhow::Result;
use clubcraft_core::{Mix, MixTimeline, TrackId};
use std::sync::Arc;

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

/// Plays a [`Mix`] in real time through two decks; supports play / pause / seek and editing
/// the mix while it plays ([`MixPlayer::update`] re-sends the schedule at the current time).
pub struct MixPlayer {
    mix: Mix,
    tracks: Vec<Arc<LoadedTrack>>,
    decks: [DeckId; 2],
    timeline: MixTimeline,
}

impl MixPlayer {
    pub fn new(mix: &Mix, tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>, decks: [DeckId; 2]) -> Result<Self> {
        let s = MixSchedule::new(mix, tracks, decks)?;
        Ok(MixPlayer { mix: mix.clone(), timeline: s.timeline.clone(), tracks: s.tracks, decks })
    }

    pub fn timeline(&self) -> &MixTimeline {
        &self.timeline
    }

    pub fn mix(&self) -> &Mix {
        &self.mix
    }

    fn schedule(&self) -> Result<MixSchedule> {
        let ids: Vec<TrackId> = self.mix.tracks.iter().map(|t| t.track_id).collect();
        MixSchedule::new(&self.mix, &|id| ids.iter().position(|x| *x == id).map(|i| self.tracks[i].clone()), self.decks)
    }

    /// Start playing at mix time `from` (s).
    pub fn play(&self, sink: &mut dyn CommandSink, from: f64) -> Result<()> {
        sink.submit(Command::PlayMix { schedule: Box::new(self.schedule()?), start: from })
    }

    pub fn seek(&self, sink: &mut dyn CommandSink, t: f64) -> Result<()> {
        sink.submit(Command::MixSeek(t))
    }

    pub fn pause(&self, sink: &mut dyn CommandSink) -> Result<()> {
        sink.submit(Command::MixPause)
    }

    pub fn resume(&self, sink: &mut dyn CommandSink) -> Result<()> {
        sink.submit(Command::MixResume)
    }

    pub fn stop(&self, sink: &mut dyn CommandSink) -> Result<()> {
        sink.submit(Command::MixStop)
    }

    /// Replace the mix (after an edit). If `state` shows it playing, playback continues at the
    /// same mix time with the new schedule.
    pub fn update(&mut self, mix: &Mix, tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>, sink: &mut dyn CommandSink, state: &EngineState) -> Result<()> {
        let s = MixSchedule::new(mix, tracks, self.decks)?;
        self.mix = mix.clone();
        self.timeline = s.timeline.clone();
        self.tracks = s.tracks.clone();
        if state.mix.active && state.mix.playing {
            sink.submit(Command::PlayMix { schedule: Box::new(s), start: state.mix.time })?;
        }
        Ok(())
    }

    /// Current mix time from an engine snapshot.
    pub fn position(state: &EngineState) -> f64 {
        state.mix.time
    }
}
