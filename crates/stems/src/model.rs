//! Model manifests (`models/<name>.json`, written by `tools/model-export/export_htdemucs.py`).

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub architecture: String,
    pub sources: Vec<String>,
    pub samplerate: u32,
    pub audio_channels: usize,
    pub segment_samples: usize,
    pub spec_frames: usize,
    pub nfft: usize,
    pub hop_length: usize,
    /// Base names of the sub-models (`<name>.mlmodelc` / `<name>.onnx`).
    pub models: Vec<String>,
    /// `weights[i][s]`: contribution of model `i` to source `s` (Demucs `BagOfModels`).
    pub weights: Vec<Vec<f32>>,
    #[serde(skip)]
    pub dir: PathBuf,
}

impl Manifest {
    pub fn load(dir: &Path, name: &str) -> Result<Manifest> {
        let p = dir.join(format!("{name}.json"));
        let text = std::fs::read_to_string(&p).with_context(|| format!("model manifest {}", p.display()))?;
        let mut m: Manifest = serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
        m.dir = dir.to_path_buf();
        if m.architecture != "htdemucs" {
            bail!("unsupported architecture {}", m.architecture);
        }
        if m.audio_channels != 2 || m.weights.len() != m.models.len() {
            bail!("invalid manifest {}", p.display());
        }
        if m.weights.iter().any(|w| w.len() != m.sources.len()) {
            bail!("invalid weights in {}", p.display());
        }
        if m.spec_frames != m.segment_samples.div_ceil(m.hop_length) || m.hop_length * 4 != m.nfft {
            bail!("inconsistent spectrogram geometry in {}", p.display());
        }
        Ok(m)
    }

    pub fn freqs(&self) -> usize {
        self.nfft / 2
    }

    /// Floats in one `spec` input: `[4][F][T]`.
    pub fn spec_len(&self) -> usize {
        2 * self.audio_channels * self.freqs() * self.spec_frames
    }

    /// Floats in one `mix` input: `[2][L]`.
    pub fn mix_len(&self) -> usize {
        self.audio_channels * self.segment_samples
    }

    /// Sources model `i` contributes to (non-zero weight).
    pub fn used_sources(&self, i: usize) -> Vec<usize> {
        (0..self.sources.len()).filter(|&s| self.weights[i][s] != 0.0).collect()
    }

    /// Map model source index -> output stem index (drums, bass, other, vocals). Guitar and piano
    /// (6-source model) fold into "other".
    pub fn stem_of_source(&self, s: usize) -> Result<usize> {
        Ok(match self.sources[s].as_str() {
            "drums" => 0,
            "bass" => 1,
            "other" | "guitar" | "piano" => 2,
            "vocals" => 3,
            x => bail!("unknown source {x}"),
        })
    }
}
