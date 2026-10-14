//! Sample lanes: audio files placed on the studio timeline that play at their own speed on top
//! of the decks (one-shots, loops, FX). Decoded once per file (process-wide cache) on the thread
//! that builds the schedule, mixed on the audio thread before the master limiter.

use clubcraft_audio::AudioBuffer;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Longest sample decoded (longer files are cut): keeps memory bounded.
const MAX_SECONDS: f64 = 120.0;

/// A sample clip as the audio thread plays it.
#[derive(Clone)]
pub struct SchedSample {
    pub lane: usize,
    /// Mix time (s) where it starts.
    pub start: f64,
    /// Linear gain (clip gain × lane fader, 0 when the lane is muted / not soloed).
    pub gain: f32,
    pub audio: Option<Arc<AudioBuffer>>,
}

impl SchedSample {
    pub fn duration(&self) -> f64 {
        self.audio.as_ref().map(|a| a.frames() as f64 / a.sample_rate as f64).unwrap_or(0.0)
    }
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Option<Arc<AudioBuffer>>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Option<Arc<AudioBuffer>>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Decoded audio of a sample file (cached; `None` if it can't be read). Not for the audio thread.
pub fn load(path: &Path) -> Option<Arc<AudioBuffer>> {
    if let Some(a) = cache().lock().get(path) {
        return a.clone();
    }
    let a = match clubcraft_audio::decode_file(path, clubcraft_core::SAMPLE_RATE) {
        Ok(mut b) => {
            let max = (MAX_SECONDS * b.sample_rate as f64) as usize;
            b.left.truncate(max);
            b.right.truncate(max);
            Some(Arc::new(b))
        }
        Err(e) => {
            log::warn!("sample {}: {e:#}", path.display());
            None
        }
    };
    cache().lock().insert(path.to_path_buf(), a.clone());
    a
}

/// Mix the samples sounding in `[t0, t0 + n / sr)` into `out` (interleaved stereo, `n` frames,
/// added). Linear interpolation when the file's rate differs from the output's. Returns whether
/// anything was written.
pub fn render(samples: &[SchedSample], t0: f64, n: usize, sr: f64, out: &mut [f32]) -> bool {
    let t1 = t0 + n as f64 / sr;
    let mut any = false;
    for s in samples {
        let Some(a) = &s.audio else { continue };
        if s.gain <= 0.0 || t1 <= s.start || t0 >= s.start + s.duration() {
            continue;
        }
        let ratio = a.sample_rate as f64 / sr;
        let len = a.frames();
        // whole output frames since the sample's start (rounded: sample-accurate, no drift)
        let off = ((t0 - s.start) * sr).round();
        for f in 0..n {
            let src = (off + f as f64) * ratio;
            if src < 0.0 {
                continue;
            }
            let i = src as usize;
            if i + 1 >= len {
                break;
            }
            let fr = (src - i as f64) as f32;
            out[2 * f] += (a.left[i] + (a.left[i + 1] - a.left[i]) * fr) * s.gain;
            out[2 * f + 1] += (a.right[i] + (a.right[i + 1] - a.right[i]) * fr) * s.gain;
            any = true;
        }
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_at_its_start_time_and_rate() {
        let a = Arc::new(AudioBuffer::new(44_100, vec![1.0; 44_100], vec![0.5; 44_100]));
        let s = vec![SchedSample { lane: 0, start: 1.0, gain: 0.5, audio: Some(a) }];
        let sr = 48_000.0;
        // block that starts 10 ms before the sample
        let mut out = vec![0.0f32; 2 * 960];
        assert!(render(&s, 0.99, 960, sr, &mut out));
        let first = out.chunks(2).position(|f| f[0] != 0.0).unwrap();
        assert_eq!(first, 480); // 10 ms at 48 kHz
        assert!((out[2 * 600] - 0.5).abs() < 1e-6 && (out[2 * 600 + 1] - 0.25).abs() < 1e-6);
        // after its end (1 s long) nothing
        let mut out = vec![0.0f32; 2 * 256];
        assert!(!render(&s, 2.5, 256, sr, &mut out));
    }
}
