//! Keep the audio of the tracks in use resident in RAM.
//!
//! Under memory pressure the OS compresses or swaps out pages that have not been touched for a
//! while: the part of a track ahead of the playhead, or the stems of a track that only plays
//! them in its next transition. The audio thread then stalls on the page fault and the output
//! stutters. [`MemoryPinner`] locks (`mlock`) the decoded audio and the stem chunks of every
//! track in use, and unlocks them once nothing plays the track any more. It runs on an
//! ordinary thread: locking faults the pages in there, never on the audio thread.

use crate::track::LoadedTrack;
use std::collections::HashMap;
use std::sync::Arc;

/// Bytes locked right now (all pinners), for diagnostics.
pub static PINNED_BYTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Upper bound for locked memory (about six tracks with stems).
pub const PIN_BUDGET: usize = 2 * 1024 * 1024 * 1024;

struct Pinned {
    /// Held so the memory is unlocked before it is freed (the allocator reuses freed memory).
    track: Arc<LoadedTrack>,
    chunks: Vec<bool>,
    bytes: usize,
}

#[derive(Default)]
pub struct MemoryPinner {
    pinned: HashMap<usize, Pinned>,
    bytes: usize,
    failed: bool,
}

impl MemoryPinner {
    /// Bytes currently locked.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Whether `t` is currently locked by this pinner.
    pub fn holds(&self, t: &Arc<LoadedTrack>) -> bool {
        self.pinned.contains_key(&key(t))
    }

    /// Lock every track in `in_use` (and stem chunks that became ready since), unlock the
    /// tracks that are no longer in use.
    pub fn update(&mut self, in_use: &[Arc<LoadedTrack>]) {
        let keep: std::collections::HashSet<usize> = in_use.iter().map(key).collect();
        let gone: Vec<usize> = self.pinned.keys().copied().filter(|k| !keep.contains(k)).collect();
        for k in gone {
            if let Some(p) = self.pinned.remove(&k) {
                unlock_track(&p.track, &p.chunks);
                self.bytes -= p.bytes;
            }
        }
        for t in in_use {
            let k = key(t);
            if !self.pinned.contains_key(&k) {
                let audio = t.audio.frames() * 8;
                if self.bytes + audio > PIN_BUDGET {
                    continue;
                }
                let ok = lock(&t.audio.left) & lock(&t.audio.right);
                self.note(ok);
                let n = t.stems.as_ref().map(|s| s.num_chunks()).unwrap_or(0);
                self.pinned.insert(k, Pinned { track: t.clone(), chunks: vec![false; n], bytes: audio });
                self.bytes += audio;
            }
            let p = self.pinned.get_mut(&k).unwrap();
            let Some(st) = t.stems.as_ref() else { continue };
            for (i, done) in p.chunks.iter_mut().enumerate() {
                if *done {
                    continue;
                }
                let Some(c) = st.chunk(i) else { continue };
                let b = c.data.len() * 2;
                if self.bytes + b > PIN_BUDGET {
                    break;
                }
                let ok = lock(&c.data);
                self.failed |= !ok;
                *done = true;
                p.bytes += b;
                self.bytes += b;
            }
        }
        PINNED_BYTES.store(self.bytes, std::sync::atomic::Ordering::Relaxed);
    }

    fn note(&mut self, ok: bool) {
        if !ok && !self.failed {
            self.failed = true;
            log::warn!("could not lock track audio in RAM (it may stutter under memory pressure)");
        }
    }
}

impl Drop for MemoryPinner {
    fn drop(&mut self) {
        for p in self.pinned.values() {
            unlock_track(&p.track, &p.chunks);
        }
    }
}

fn key(t: &Arc<LoadedTrack>) -> usize {
    Arc::as_ptr(t) as usize
}

fn unlock_track(t: &LoadedTrack, chunks: &[bool]) {
    unlock(&t.audio.left);
    unlock(&t.audio.right);
    if let Some(st) = t.stems.as_ref() {
        for (i, done) in chunks.iter().enumerate() {
            if let (true, Some(c)) = (*done, st.chunk(i)) {
                unlock(&c.data);
            }
        }
    }
}

fn lock<T>(v: &[T]) -> bool {
    if v.is_empty() {
        return true;
    }
    #[cfg(unix)]
    {
        // SAFETY: the range is a live allocation; mlock only changes its residency.
        unsafe { libc::mlock(v.as_ptr().cast(), std::mem::size_of_val(v)) == 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn unlock<T>(v: &[T]) {
    if v.is_empty() {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: as in `lock`.
        unsafe {
            libc::munlock(v.as_ptr().cast(), std::mem::size_of_val(v));
        }
    }
}
