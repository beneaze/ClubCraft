//! Extract a range of a track, the full sound or chosen stems, as a WAV file, the way the mix
//! plays it: stretched to the mix tempo and transposed by the track's key shift, so a copy placed
//! on a sample lane lines up with the original.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Full,
    Drums,
    Bass,
    Melody,
    Vocals,
    Instrumental,
    DrumsBass,
}

impl Part {
    pub const ALL: [Part; 7] = [Part::Full, Part::Drums, Part::Bass, Part::Melody, Part::Vocals, Part::Instrumental, Part::DrumsBass];

    pub fn label(self) -> &'static str {
        match self {
            Part::Full => "Full sound",
            Part::Drums => "Drums",
            Part::Bass => "Bass",
            Part::Melody => "Melody",
            Part::Vocals => "Vocals",
            Part::Instrumental => "Instrumental",
            Part::DrumsBass => "Drums + bass",
        }
    }

    /// Stem weights (drums, bass, other, vocals); `None` = the original audio.
    pub fn weights(self) -> Option<[f32; 4]> {
        match self {
            Part::Full => None,
            Part::Drums => Some([1.0, 0.0, 0.0, 0.0]),
            Part::Bass => Some([0.0, 1.0, 0.0, 0.0]),
            Part::Melody => Some([0.0, 0.0, 1.0, 0.0]),
            Part::Vocals => Some([0.0, 0.0, 0.0, 1.0]),
            Part::Instrumental => Some([1.0, 1.0, 1.0, 0.0]),
            Part::DrumsBass => Some([1.0, 1.0, 0.0, 0.0]),
        }
    }
}

pub struct Request {
    pub audio_path: PathBuf,
    pub hash: String,
    /// Range in track seconds.
    pub t0: f64,
    pub t1: f64,
    /// Playback speed in the mix (mix tempo / track tempo).
    pub rate: f64,
    /// Transposition in semitones (key shift, plus varispeed without keylock).
    pub semis: f32,
    /// Stem weights (drums, bass, other, vocals); `None` = the original audio.
    pub weights: Option<[f32; 4]>,
    pub out: PathBuf,
}

const SR: f64 = clubcraft_core::SAMPLE_RATE as f64;

/// Write the WAV; returns its length in seconds.
pub fn run(r: &Request) -> Result<f64> {
    if r.t1 <= r.t0 {
        bail!("empty range");
    }
    // pre-roll for the stretcher, read with the range
    let pre = 0.25;
    let a = ((r.t0 - pre).max(0.0) * SR) as usize;
    let pre_frames = (r.t0 * SR) as usize - a;
    let b = (r.t1 * SR) as usize;
    let (mut l, mut rr) = (Vec::with_capacity(b - a), Vec::with_capacity(b - a));
    match r.weights {
        None => {
            let audio = clubcraft_audio::decode_file(&r.audio_path, clubcraft_core::SAMPLE_RATE)?;
            let b = b.min(audio.frames());
            l.extend_from_slice(&audio.left[a.min(b)..b]);
            rr.extend_from_slice(&audio.right[a.min(b)..b]);
        }
        Some(w) => {
            let p = clubcraft_core::library::stems_path(&r.hash);
            if !p.exists() {
                bail!("this track has no stems yet");
            }
            let st = clubcraft_stems::load_stem_track(&p).context("load stems")?;
            for j in a..b.min(st.frames) {
                let f = st.frame(j).unwrap_or([0.0; 8]);
                l.push((0..4).map(|k| w[k] * f[2 * k]).sum());
                rr.push((0..4).map(|k| w[k] * f[2 * k + 1]).sum());
            }
        }
    }
    let n = l.len();
    let mut inter: Vec<f32> = (0..n).flat_map(|i| [l[i], rr[i]]).collect();
    // as the mix plays it
    if (r.rate - 1.0).abs() > 1e-4 || r.semis.abs() > 1e-3 {
        let mut s = clubcraft_engine::stretch::Stretcher::new(2, (SR * 0.12) as usize, (SR * 0.03) as usize, false);
        s.set_transpose_factor(2f32.powf(r.semis / 12.0));
        let (il, ol) = (s.input_latency(), s.output_latency());
        let start = pre_frames.saturating_sub(il);
        s.seek(&inter[2 * start..2 * pre_frames], r.rate);
        let body = &inter[2 * pre_frames..];
        let out_frames = ((body.len() / 2) as f64 / r.rate).round() as usize + ol;
        let mut padded = body.to_vec();
        padded.resize((((out_frames as f64) * r.rate).ceil() as usize) * 2, 0.0);
        let mut out = vec![0f32; out_frames * 2];
        s.process(&padded, &mut out);
        out.drain(..ol * 2);
        inter = out;
    } else {
        inter.drain(..2 * pre_frames);
    }
    // 5 ms fades against clicks
    let frames = inter.len() / 2;
    let fade = ((SR * 0.005) as usize).min(frames / 2);
    for i in 0..fade {
        let g = i as f32 / fade as f32;
        for c in 0..2 {
            inter[2 * i + c] *= g;
            inter[2 * (frames - 1 - i) + c] *= g;
        }
    }
    if let Some(dir) = r.out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut w = clubcraft_engine::wav::WavSink::create(&r.out, clubcraft_core::SAMPLE_RATE, clubcraft_engine::WavFormat::Pcm24)?;
    w.write(&inter)?;
    w.finish()?;
    Ok(frames as f64 / SR)
}
