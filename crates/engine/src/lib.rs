//! # clubcraft-engine
//!
//! The real-time audio engine of ClubCraft (VirtualDJ-style live decks) and the offline mix
//! renderer (DJ.Studio-style Studio export). Everything runs on the same DSP:
//!
//! | module | what |
//! |---|---|
//! | [`deck`] | [`Deck`]: transport, CDJ/hold/CUP cue logic, 8 hot cues, pitch ranges, keylock & key shift (Signalsmith Stretch), Hermite varispeed, scratch/jog, reverse, brake, spinback, slip, loops / loop roll / beat jump, quantize, stems (residual formula, progressive), stem echo |
//! | [`mixer`] | channel strip (trim + auto-gain, LR4 isolator EQ, one-knob filter, 3 FX slots, fader curves, crossfader curves/assign, PFL, VU) and master (gain, true-peak limiter, headphone cue/master blend) |
//! | [`fx`] | beat-synced echo / ping-pong, Freeverb reverb, flanger, filter LFO, trance gate, beat roll, bitcrusher, brake |
//! | [`core`] | [`EngineCore`]: decks + mixer + sync (master selection, tempo + continuous phase lock) + mix conductor + recorder tap; offline-usable |
//! | [`engine`] | [`Engine::start`] → [`EngineHandle`]: cpal output, lock-free command ring, triple-buffered [`EngineState`], headphone routing, device list, recording |
//! | [`command`] / [`state`] | the UI ↔ audio protocol: [`Command`] and [`EngineState`] |
//! | [`conductor`] / [`automix`] / [`player`] | play a [`clubcraft_core::Mix`] (or a playlist) live through two decks along the core [`clubcraft_core::MixTimeline`]; [`MixPlayer`] = studio preview with seeking |
//! | [`render`] | [`render_mix`]: offline WAV export, faster than real time |
//! | [`midi`] | controller input via midir, JSON mappings with VirtualDJ-like verbs, learn mode |
//!
//! ## Quick start
//! ```no_run
//! use clubcraft_engine::*;
//! use std::sync::Arc;
//! # fn main() -> anyhow::Result<()> {
//! let engine = Engine::start(AudioConfig::default())?;
//! let audio = Arc::new(clubcraft_audio::decode_file("track.mp3".as_ref(), clubcraft_core::SAMPLE_RATE)?);
//! engine.send(Command::LoadTrack(0, Arc::new(LoadedTrack::new("track", audio))))?;
//! engine.send(Command::Play(0))?;
//! let st = engine.state();
//! println!("{:.2} s", st.decks[0].position);
//! # Ok(()) }
//! ```

pub mod automix;
pub mod command;
pub mod conductor;
pub mod core;
pub mod deck;
pub mod dsp;
pub mod engine;
pub mod fx;
pub mod midi;
pub mod mixer;
pub mod player;
pub mod render;
pub mod samples;
pub mod state;
#[allow(unsafe_code)] // FFI to the vendored Signalsmith Stretch
pub mod stretch;
#[allow(unsafe_code)] // the few OS calls: page-fault counter, mlock
mod sys;
pub mod track;
pub mod wav;

pub use automix::AutomixOptions;
pub use command::{Command, DeckId, EqBand};
pub use conductor::{window_at, MixSchedule, MIX_WINDOW};
pub use core::EngineCore;
pub use deck::Deck;
pub use engine::{devices, AudioConfig, CommandSender, DeviceInfo, Engine, EngineHandle, HeadphoneRouting, StreamInfo};
pub use fx::FxKind;
pub use mixer::{CrossfaderAssign, CrossfaderCurve, FaderCurve};
pub use player::{CommandSink, MixPlayer, PrefetchStatus, TrackLoader};
pub use render::{render_mix, render_mix_to_vec, RenderOptions, RenderReport};
pub use state::*;
pub mod pin;
pub mod prof;
pub use track::LoadedTrack;
pub use wav::WavFormat;
pub use clubcraft_core::{MixTimeline, ActiveTrack, TimelineTrack, TimelineTransition};
