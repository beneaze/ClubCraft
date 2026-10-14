//! Live automix: turns a playlist of analysed tracks into a mix (mix-in/out points from the
//! analysis cues, transition style per pair) and plays it through two decks with the
//! [`crate::conductor`]: the next track is preloaded on the idle deck, tempo-matched, started so
//! that its `MixIn` lands exactly on the outgoing `MixOut` beat, the transition lanes are applied
//! over the transition beats, then the roles swap. A studio [`Mix`] is played live the same way
//! (see [`crate::MixPlayer`]).

use crate::command::{Command, DeckId};
use crate::conductor::MixSchedule;
use crate::track::LoadedTrack;
use anyhow::{bail, Result};
use clubcraft_core::{Mix, MixTrack, TempoMode, Transition, TransitionStyle};
use std::sync::Arc;

/// Automix settings.
#[derive(Clone, Debug)]
pub struct AutomixOptions {
    /// Preferred transition style (stem styles fall back to bass swap for tracks without stems).
    pub style: TransitionStyle,
    /// Transition length in beats.
    pub beats: u32,
    pub tempo: TempoMode,
    pub keylock: bool,
    /// Deck for the first track and the deck for the second.
    pub decks: [DeckId; 2],
}

impl Default for AutomixOptions {
    fn default() -> Self {
        AutomixOptions { style: TransitionStyle::StemBlend, beats: 32, tempo: TempoMode::Glide, keylock: true, decks: [0, 1] }
    }
}

fn snap_to_beat(t: &LoadedTrack, pos: f64) -> f64 {
    match t.grid {
        Some(g) => g.snap(pos, 1.0).max(0.0),
        None => pos,
    }
}

fn uses_stems(s: TransitionStyle) -> bool {
    matches!(s, TransitionStyle::StemBlend | TransitionStyle::VocalOver)
}

/// Build a mix from a playlist. Track ids in the returned mix are playlist indices + 1.
pub fn plan(playlist: &[Arc<LoadedTrack>], opts: &AutomixOptions) -> Mix {
    let mut mix = Mix { name: "Automix".into(), tempo: opts.tempo, keylock: opts.keylock, ..Default::default() };
    for (i, t) in playlist.iter().enumerate() {
        let mix_in = snap_to_beat(t, t.mix_in());
        let mut mix_out = snap_to_beat(t, t.mix_out(opts.beats));
        let bl = t.grid.map(|g| g.beat_len()).unwrap_or(0.5);
        if mix_out < mix_in + 8.0 * bl {
            mix_out = (t.duration() - opts.beats as f64 * bl).max(mix_in);
        }
        mix.tracks.push(MixTrack { track_id: i as i64 + 1, mix_in, mix_out, key_shift: 0, gain_db: 0.0 });
    }
    for w in playlist.windows(2) {
        let both_stems = w[0].stems.is_some() && w[1].stems.is_some();
        let style = if uses_stems(opts.style) && !both_stems { TransitionStyle::BassSwap } else { opts.style };
        mix.transitions.push(Transition::new(style, opts.beats));
    }
    mix
}

/// Schedule for a playlist (see [`plan`]).
pub fn schedule(playlist: &[Arc<LoadedTrack>], opts: &AutomixOptions) -> Result<MixSchedule> {
    if playlist.is_empty() {
        bail!("empty playlist");
    }
    let mix = plan(playlist, opts);
    MixSchedule::new(&mix, &|id| playlist.get((id - 1) as usize).cloned(), opts.decks)
}

/// The command that starts automix. `from = Some((track index, position s))` continues from a
/// track that is already playing (e.g. the current deck) instead of starting at the top.
pub fn start(playlist: &[Arc<LoadedTrack>], opts: &AutomixOptions, from: Option<(usize, f64)>) -> Result<Command> {
    let s = schedule(playlist, opts)?;
    let start = match from {
        Some((i, pos)) if i < s.len() => s.time_of_track_pos(i, pos).max(0.0),
        _ => 0.0,
    };
    Ok(Command::PlayMix { schedule: Box::new(s), start })
}
