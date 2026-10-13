//! ClubCraft core: the data model shared by analysis, stems, the live engine and the studio.
//!
//! Units: all positions are **seconds** in the original (unstretched) track timeline unless a
//! field says otherwise. Audio inside ClubCraft is 44.1 kHz stereo f32.

pub mod analysis;
pub mod key;
pub mod library;
pub mod mix;
pub mod paths;
pub mod planner;
pub mod stems;
pub mod timeline;

pub use analysis::*;
pub use key::Key;
pub use library::{Library, Track, TrackId};
pub use mix::*;
pub use timeline::{ActiveTrack, MixTimeline, TimelineTrack, TimelineTransition};
pub use stems::{StemTrack, STEM_CHANNELS, STEM_CHUNK_FRAMES};

/// Internal processing sample rate (also the rate the Demucs models are trained on).
pub const SAMPLE_RATE: u32 = 44_100;
