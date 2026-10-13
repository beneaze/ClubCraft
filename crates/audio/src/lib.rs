//! ClubCraft audio I/O: decoding any common format to 44.1 kHz stereo f32, metadata, resampling.

pub mod decode;
pub mod meta;
pub mod resample;

pub use decode::{decode_file, AudioBuffer};
pub use meta::{read_metadata, Metadata};
