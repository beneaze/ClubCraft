//! Shared, progressively filled stem buffer.
//!
//! The separator fills chunks (in any order) while the live engine reads them lock-free from the
//! audio thread. A chunk that is not ready yet simply reads as "no stems here", and the engine
//! falls back to the original audio for that region.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

/// Frames per chunk (2^16 = 1.49 s at 44.1 kHz).
pub const STEM_CHUNK_FRAMES: usize = 1 << 16;
/// Interleaved channels per frame: 4 stems x stereo, in [`crate::StemKind`] order:
/// drums L, drums R, bass L, bass R, other L, other R, vocals L, vocals R.
pub const STEM_CHANNELS: usize = 8;

pub struct StemChunk {
    /// `frames * STEM_CHANNELS` interleaved samples.
    pub data: Vec<f32>,
}

pub struct StemTrack {
    pub frames: usize,
    /// Name of the model that produced the stems (e.g. "htdemucs_ft").
    pub model: String,
    chunks: Vec<OnceLock<StemChunk>>,
    ready: AtomicUsize,
}

impl StemTrack {
    pub fn new(frames: usize, model: impl Into<String>) -> Self {
        let n = frames.div_ceil(STEM_CHUNK_FRAMES);
        StemTrack { frames, model: model.into(), chunks: (0..n).map(|_| OnceLock::new()).collect(), ready: AtomicUsize::new(0) }
    }

    pub fn num_chunks(&self) -> usize {
        self.chunks.len()
    }

    /// Frames covered by chunk `i` (the last chunk may be shorter).
    pub fn chunk_frames(&self, i: usize) -> usize {
        let start = i * STEM_CHUNK_FRAMES;
        (self.frames - start.min(self.frames)).min(STEM_CHUNK_FRAMES)
    }

    /// Publish chunk `i`. `data.len()` must equal `chunk_frames(i) * STEM_CHANNELS`.
    /// Publishing an already published chunk is a no-op.
    pub fn set_chunk(&self, i: usize, data: Vec<f32>) {
        debug_assert_eq!(data.len(), self.chunk_frames(i) * STEM_CHANNELS);
        if self.chunks[i].set(StemChunk { data }).is_ok() {
            self.ready.fetch_add(1, Ordering::Release);
        }
    }

    pub fn chunk(&self, i: usize) -> Option<&StemChunk> {
        self.chunks.get(i)?.get()
    }

    pub fn is_chunk_ready(&self, i: usize) -> bool {
        self.chunks.get(i).is_some_and(|c| c.get().is_some())
    }

    pub fn ready_fraction(&self) -> f32 {
        if self.chunks.is_empty() {
            return 1.0;
        }
        self.ready.load(Ordering::Acquire) as f32 / self.chunks.len() as f32
    }

    pub fn is_complete(&self) -> bool {
        self.ready.load(Ordering::Acquire) == self.chunks.len()
    }

    /// The 8 interleaved stem samples at `frame`, or `None` if that region is not separated yet.
    #[inline]
    pub fn frame(&self, frame: usize) -> Option<&[f32]> {
        let c = self.chunk(frame / STEM_CHUNK_FRAMES)?;
        let o = (frame % STEM_CHUNK_FRAMES) * STEM_CHANNELS;
        c.data.get(o..o + STEM_CHANNELS)
    }

    /// Build a complete track from planar stems: `stems[s] = (left, right)` in StemKind order.
    pub fn from_planar(model: impl Into<String>, stems: &[(Vec<f32>, Vec<f32>); 4]) -> Self {
        let frames = stems[0].0.len();
        let t = StemTrack::new(frames, model);
        for i in 0..t.num_chunks() {
            let start = i * STEM_CHUNK_FRAMES;
            let n = t.chunk_frames(i);
            let mut data = Vec::with_capacity(n * STEM_CHANNELS);
            for f in start..start + n {
                for (l, r) in stems.iter() {
                    data.push(l[f]);
                    data.push(r[f]);
                }
            }
            t.set_chunk(i, data);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progressive() {
        let n = STEM_CHUNK_FRAMES + 10;
        let t = StemTrack::new(n, "x");
        assert_eq!(t.num_chunks(), 2);
        assert!(t.frame(5).is_none());
        t.set_chunk(1, vec![0.5; 10 * STEM_CHANNELS]);
        assert_eq!(t.frame(STEM_CHUNK_FRAMES + 3).unwrap()[7], 0.5);
        assert!(t.frame(0).is_none());
        assert_eq!(t.ready_fraction(), 0.5);
    }
}
