use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::sync::LazyLock;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Planar stereo audio. Both channels always have the same length.
#[derive(Clone, Debug, Default)]
pub struct AudioBuffer {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl AudioBuffer {
    pub fn new(sample_rate: u32, left: Vec<f32>, right: Vec<f32>) -> Self {
        debug_assert_eq!(left.len(), right.len());
        AudioBuffer { sample_rate, left, right }
    }

    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn duration(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }

    /// Mono downmix (L+R)/2.
    pub fn mono(&self) -> Vec<f32> {
        self.left.iter().zip(&self.right).map(|(l, r)| 0.5 * (l + r)).collect()
    }

    /// Interleaved L/R samples.
    pub fn interleaved(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.frames() * 2);
        for (l, r) in self.left.iter().zip(&self.right) {
            out.push(*l);
            out.push(*r);
        }
        out
    }

    pub fn from_interleaved(sample_rate: u32, data: &[f32]) -> Self {
        let n = data.len() / 2;
        let mut left = Vec::with_capacity(n);
        let mut right = Vec::with_capacity(n);
        for f in data.as_chunks::<2>().0 {
            left.push(f[0]);
            right.push(f[1]);
        }
        AudioBuffer { sample_rate, left, right }
    }
}

/// Symphonia's own decoders plus libopus, which Symphonia lacks. Without it `.opus` files (what
/// yt-dlp and many web sources hand out) import fine but can never be analysed or played.
static CODECS: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    registry
});

/// Decode an audio file (mp3, aac/m4a, alac, flac, wav, aiff, ogg, opus ...) to stereo f32 at
/// `target_rate` (use [`clubcraft_core::SAMPLE_RATE`]). Mono files are duplicated, multichannel
/// files are folded down to their first two channels.
pub fn decode_file(path: &Path, target_rate: u32) -> Result<AudioBuffer> {
    let buf = decode_native(path)?;
    if buf.sample_rate == target_rate {
        Ok(buf)
    } else {
        Ok(crate::resample::resample(&buf, target_rate))
    }
}

/// Decode at the file's native sample rate.
pub fn decode_native(path: &Path) -> Result<AudioBuffer> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .with_context(|| format!("unsupported format: {}", path.display()))?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| anyhow!("no audio track"))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("no audio codec parameters"))?
        .clone();
    let mut decoder = CODECS.make_audio_decoder(&params, &AudioDecoderOptions::default())?;
    let track_id = track.id;

    let mut left = Vec::new();
    let mut right = Vec::new();
    let mut rate = params.sample_rate.unwrap_or(0);
    let mut tmp: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(Error::ResetRequired) => break,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio) => {
                let spec = audio.spec();
                rate = spec.rate();
                let ch = spec.channels().count().max(1);
                tmp.resize(audio.samples_interleaved(), 0.0);
                audio.copy_to_slice_interleaved(&mut tmp);
                left.reserve(tmp.len() / ch);
                right.reserve(tmp.len() / ch);
                for f in tmp.chunks_exact(ch) {
                    left.push(f[0]);
                    right.push(if ch > 1 { f[1] } else { f[0] });
                }
            }
            Err(Error::DecodeError(e)) => log::debug!("decode error (skipped): {e}"),
            Err(e) => return Err(e.into()),
        }
    }
    if rate == 0 {
        return Err(anyhow!("unknown sample rate"));
    }
    Ok(AudioBuffer { sample_rate: rate, left, right })
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;

    #[test]
    fn opus_has_a_decoder() {
        assert!(CODECS.get_audio_decoder(CODEC_ID_OPUS).is_some());
    }
}
