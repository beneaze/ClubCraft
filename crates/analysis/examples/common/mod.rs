//! Helpers shared by the analysis examples (not part of the library API).
#![allow(dead_code)]

use clubcraft_analysis::OwnedStems;
use clubcraft_audio::AudioBuffer;
use std::path::Path;

/// Load Demucs-style stems (`drums`, `bass`, `other`, `vocals` + any audio extension) from `dir`
/// and align them to `mix` (MP3-encoded stems carry encoder delay; the lag is found by
/// cross-correlating the stem sum with the mix).
pub fn load_stems(dir: &Path, mix: &AudioBuffer) -> anyhow::Result<OwnedStems> {
    let mut stems: [(Vec<f32>, Vec<f32>); 4] = Default::default();
    for (i, name) in ["drums", "bass", "other", "vocals"].iter().enumerate() {
        let file = ["wav", "mp3", "flac"]
            .iter()
            .map(|e| dir.join(format!("{name}.{e}")))
            .find(|p| p.exists())
            .ok_or_else(|| anyhow::anyhow!("missing stem {name} in {}", dir.display()))?;
        let b = clubcraft_audio::decode_file(&file, mix.sample_rate)?;
        stems[i] = (b.left, b.right);
    }
    let n = mix.frames();
    // Find the lag (stems later than mix by `lag` samples) on a 30 s excerpt.
    let a = (n / 3).min(n.saturating_sub(1));
    let len = (30 * mix.sample_rate as usize).min(n - a);
    let sum: Vec<f32> = (0..stems[0].0.len())
        .map(|i| {
            stems
                .iter()
                .map(|s| s.0.get(i).copied().unwrap_or(0.0))
                .sum()
        })
        .collect();
    let mut best = (0isize, f64::MIN);
    for lag in -3000isize..=3000 {
        let mut acc = 0.0f64;
        for i in (a..a + len).step_by(7) {
            let j = i as isize + lag;
            if j >= 0 && (j as usize) < sum.len() {
                acc += (mix.left[i] * sum[j as usize]) as f64;
            }
        }
        if acc > best.1 {
            best = (lag, acc);
        }
    }
    let lag = best.0;
    for s in stems.iter_mut() {
        for ch in [&mut s.0, &mut s.1] {
            let mut v = vec![0.0f32; n];
            for (i, o) in v.iter_mut().enumerate() {
                let j = i as isize + lag;
                if j >= 0 && (j as usize) < ch.len() {
                    *o = ch[j as usize];
                }
            }
            *ch = v;
        }
    }
    Ok(OwnedStems { stems })
}

pub fn fmt_time(t: f64) -> String {
    let m = (t / 60.0).floor();
    format!("{}:{:06.3}", m as i64, t - m * 60.0)
}
