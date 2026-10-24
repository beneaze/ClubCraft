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
    /// Position in the file (s) at `start`, modulo the file's length.
    pub offset: f64,
    /// How long it sounds (s); past the file's end it loops.
    pub length: f64,
    pub audio: Option<Arc<AudioBuffer>>,
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
        let len = a.frames();
        if s.gain <= 0.0 || len < 2 || t1 <= s.start || t0 >= s.start + s.length {
            continue;
        }
        let ratio = a.sample_rate as f64 / sr;
        // whole output frames since the sample's start (rounded: sample-accurate, no drift)
        let off = ((t0 - s.start) * sr).round();
        let (head, end) = (s.offset * a.sample_rate as f64, s.length * sr);
        for f in 0..n {
            let since = off + f as f64;
            if since < 0.0 {
                continue;
            }
            if since >= end {
                break;
            }
            // a clip longer than its file loops it; the last frame blends into the first
            let src = (head + since * ratio).rem_euclid(len as f64);
            let i = (src as usize).min(len - 1);
            let k = if i + 1 < len { i + 1 } else { 0 };
            let fr = (src - i as f64) as f32;
            out[2 * f] += (a.left[i] + (a.left[k] - a.left[i]) * fr) * s.gain;
            out[2 * f + 1] += (a.right[i] + (a.right[k] - a.right[i]) * fr) * s.gain;
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
        let s = vec![SchedSample { lane: 0, start: 1.0, gain: 0.5, offset: 0.0, length: 1.0, audio: Some(a) }];
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

    #[test]
    fn extended_clips_loop_the_file_and_trimmed_ones_stop() {
        // 1 s ramp 0..1 at the output rate: the value tells the position in the file
        let sr = 48_000.0;
        let ramp: Vec<f32> = (0..48_000).map(|i| i as f32 / 48_000.0).collect();
        let a = Arc::new(AudioBuffer::new(48_000, ramp.clone(), ramp));
        // -0.75 s is 0.25 s into the file; lasting 2 s it plays 0.25..1, 0..1, 0..0.25
        let s = vec![SchedSample { lane: 0, start: 0.0, gain: 1.0, offset: -0.75, length: 2.0, audio: Some(a.clone()) }];
        let at = |t: f64| {
            let mut out = vec![0.0f32; 2];
            render(&s, t, 1, sr, &mut out);
            out[0]
        };
        assert!((at(0.0) - 0.25).abs() < 1e-3);
        assert!((at(1.0) - 0.25).abs() < 1e-3);
        assert!((at(1.5) - 0.75).abs() < 1e-3);
        let mut out = vec![0.0f32; 2 * 256];
        assert!(!render(&s, 2.0, 256, sr, &mut out));
        // trimmed to 0.5 s: silent after that
        let s = vec![SchedSample { lane: 0, start: 0.0, gain: 1.0, offset: 0.0, length: 0.5, audio: Some(a) }];
        let mut out = vec![0.0f32; 2 * 48_000];
        render(&s, 0.0, 48_000, sr, &mut out);
        assert!(out[2 * 23_999] > 0.0 && out[2 * 24_000] == 0.0);
    }
}
