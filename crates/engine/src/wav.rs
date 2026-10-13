//! WAV output (recordings and offline renders) and the recorder writer thread.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Output sample format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WavFormat {
    /// 16-bit PCM with TPDF dither.
    Pcm16,
    #[default]
    Pcm24,
    Float32,
}

/// Streaming stereo WAV writer.
pub struct WavSink {
    w: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    format: WavFormat,
    seed: u32,
    frames: u64,
}

impl WavSink {
    pub fn create(path: &Path, sample_rate: u32, format: WavFormat) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate,
            bits_per_sample: match format {
                WavFormat::Pcm16 => 16,
                WavFormat::Pcm24 => 24,
                WavFormat::Float32 => 32,
            },
            sample_format: if format == WavFormat::Float32 { hound::SampleFormat::Float } else { hound::SampleFormat::Int },
        };
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).ok();
            }
        }
        let w = hound::WavWriter::create(path, spec).with_context(|| format!("create {}", path.display()))?;
        Ok(WavSink { w, format, seed: 0x1234_5678, frames: 0 })
    }

    #[inline]
    fn rand(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.seed >> 8) as f32 / (1u32 << 24) as f32
    }

    /// Write interleaved stereo samples.
    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        match self.format {
            WavFormat::Float32 => {
                for s in samples {
                    self.w.write_sample(s.clamp(-1.0, 1.0))?;
                }
            }
            WavFormat::Pcm16 => {
                for s in samples {
                    let d = self.rand() - self.rand();
                    let v = (s.clamp(-1.0, 1.0) * 32767.0 + d).round().clamp(-32768.0, 32767.0) as i16;
                    self.w.write_sample(v)?;
                }
            }
            WavFormat::Pcm24 => {
                for s in samples {
                    let v = (s.clamp(-1.0, 1.0) as f64 * 8_388_607.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32;
                    self.w.write_sample(v)?;
                }
            }
        }
        self.frames += (samples.len() / 2) as u64;
        Ok(())
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn finish(self) -> Result<()> {
        self.w.finalize()?;
        Ok(())
    }
}

/// Recorder writer thread: drains the ring filled by the audio thread and writes a WAV file.
/// Finishes the file when the producer side is dropped.
pub(crate) struct RecorderThread {
    pub path: PathBuf,
    pub join: std::thread::JoinHandle<Result<u64>>,
}

impl RecorderThread {
    pub fn spawn(path: PathBuf, sample_rate: u32, format: WavFormat, mut cons: rtrb::Consumer<f32>) -> Result<Self> {
        let mut sink = WavSink::create(&path, sample_rate, format)?;
        let join = std::thread::Builder::new().name("clubcraft-recorder".into()).spawn(move || -> Result<u64> {
            let mut tmp = vec![0f32; 8192];
            loop {
                let avail = cons.slots();
                if avail == 0 {
                    if cons.is_abandoned() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
                let n = (avail.min(tmp.len()) / 2) * 2;
                if n == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                if let Ok(chunk) = cons.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    tmp[..a.len()].copy_from_slice(a);
                    tmp[a.len()..a.len() + b.len()].copy_from_slice(b);
                    chunk.commit_all();
                    sink.write(&tmp[..n])?;
                }
            }
            let frames = sink.frames();
            sink.finish()?;
            Ok(frames)
        })?;
        Ok(RecorderThread { path, join })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_writes_master() {
        let dir = std::env::temp_dir().join("clubcraft-engine-rec-test");
        let path = dir.join("rec.wav");
        let (mut p, c) = rtrb::RingBuffer::<f32>::new(44100);
        let t = RecorderThread::spawn(path.clone(), 44100, WavFormat::Float32, c).unwrap();
        for i in 0..20000 {
            while p.push((i as f32 * 0.01).sin() * 0.5).is_err() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        drop(p);
        let frames = t.join.join().unwrap().unwrap();
        assert_eq!(frames, 10000);
        let r = hound::WavReader::open(&path).unwrap();
        assert_eq!(r.duration(), 10000);
        std::fs::remove_file(&path).ok();
    }
}
