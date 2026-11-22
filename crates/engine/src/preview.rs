//! Isolated preview decks. Rendered by the main callback, never enrolled in DJ sync.
use crate::conductor::{Conductor, MixSchedule};
use crate::deck::Deck;
use crate::dsp::MAX_BLOCK;
use crate::fx::FxCtx;
use crate::mixer::Mixer;
use crate::state::MixPlaybackState;
use crate::track::LoadedTrack;
use std::sync::Arc;

pub(crate) struct PreviewRenderer {
    pub conductor: Conductor,
    pub decks: Vec<Deck>,
    pub mixer: Mixer,
    buffers: Vec<Vec<f32>>,
    cue: Vec<f32>,
}

impl PreviewRenderer {
    pub fn new(sr: u32) -> Self {
        let mixer = Mixer::new(sr as f32, 2);
        Self {
            conductor: Conductor::default(),
            decks: (0..2).map(|_| Deck::new(sr)).collect(),
            mixer,
            buffers: (0..2).map(|_| vec![0.0; MAX_BLOCK * 2]).collect(),
            cue: vec![0.0; MAX_BLOCK * 2],
        }
    }

    pub fn set_sample_rate(&mut self, sr: u32) {
        self.mixer = Mixer::new(sr as f32, 2);
        for deck in &mut self.decks {
            deck.set_output_rate(sr);
        }
    }

    pub fn start(
        &mut self,
        schedule: Box<MixSchedule>,
        start: f64,
        drop_track: &mut dyn FnMut(Arc<LoadedTrack>),
    ) -> Option<Box<MixSchedule>> {
        for deck in &mut self.decks {
            deck.clear_preview_history();
        }
        for channel in &mut self.mixer.channels {
            channel.clear_preview_history();
        }
        self.conductor.start(
            schedule,
            start,
            &mut self.decks,
            &mut self.mixer,
            drop_track,
        )
    }

    pub fn stop(
        &mut self,
        drop_track: &mut dyn FnMut(Arc<LoadedTrack>),
    ) -> Option<Box<MixSchedule>> {
        let old = self
            .conductor
            .stop(&mut self.decks, &mut self.mixer, drop_track);
        for deck in &mut self.decks {
            if let Some(track) = deck.unload() {
                drop_track(track);
            }
            deck.clear_preview_history();
        }
        old
    }

    pub fn state(&self) -> MixPlaybackState {
        self.conductor.state()
    }

    pub fn process(
        &mut self,
        sr: u32,
        n: usize,
        out: &mut [f32],
        drop_track: &mut dyn FnMut(Arc<LoadedTrack>),
    ) {
        out[..n * 2].fill(0.0);
        self.conductor
            .step(&mut self.decks, &mut self.mixer, n, sr as f64, drop_track);
        if self.conductor.sched.is_none() || self.conductor.finished {
            return;
        }
        let ctxs = std::array::from_fn::<_, 2, _>(|d| {
            let deck = &self.decks[d];
            let bpm = deck.effective_bpm().unwrap_or(120.0).max(1.0);
            FxCtx {
                sr: sr as f32,
                bpm,
                beat: if deck.is_playing() {
                    deck.beat_position()
                } else {
                    None
                },
                beats_per_frame: bpm / 60.0 / sr as f64,
            }
        });
        for (deck, buf) in self.decks.iter_mut().zip(&mut self.buffers) {
            deck.process(&mut buf[..n * 2]);
        }
        // Final gain/limiting belongs to the selected destination, not this private mixer.
        self.mixer
            .process_channels(&mut self.buffers, &ctxs, 2, n, out, &mut self.cue);
    }
}
