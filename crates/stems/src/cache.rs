//! Stem file cache: **8-channel 16-bit FLAC** (`<hash>.stems.flac`, see
//! [`clubcraft_core::library::stems_path`]).
//!
//! Channels are interleaved in [`clubcraft_core::STEM_CHANNELS`] order (drums L/R, bass L/R,
//! other L/R, vocals L/R). Stems can peak above 0 dBFS (they are not limited like the master), so
//! samples are stored as `round(x / scale * 32768)` with `scale = max(1, peak)` recorded in a
//! Vorbis comment (`CLUBCRAFT_SCALE`), next to `CLUBCRAFT_MODEL` and `CLUBCRAFT_STEMS`. Any FLAC
//! tool can open the file; the quantisation noise floor is −98 dBFS (×scale), far below audibility,
//! and the residual-trick playback (`original + Σ(gain−1)·stem`) is unaffected in practice.
//!
//! Encoding runs multi-threaded (`flacenc`); decoding uses symphonia and fills [`StemTrack`]
//! chunks directly.

use crate::Stems;
use anyhow::{anyhow, bail, Context, Result};
use clubcraft_core::{StemTrack, SAMPLE_RATE, STEM_CHANNELS, STEM_CHUNK_FRAMES};
use flacenc::component::{BitRepr, MetadataBlockData};
use flacenc::error::Verify;
use std::path::Path;

const BLOCK: usize = 4096;

fn vorbis_comment(tags: &[(&str, String)]) -> Vec<u8> {
    let vendor = b"clubcraft-stems";
    let mut v = Vec::new();
    v.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    v.extend_from_slice(vendor);
    v.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, val) in tags {
        let s = format!("{k}={val}");
        v.extend_from_slice(&(s.len() as u32).to_le_bytes());
        v.extend_from_slice(s.as_bytes());
    }
    v
}

/// Model name recorded in a stem cache file (`CLUBCRAFT_MODEL`), reading only the FLAC metadata
/// blocks (a few hundred bytes), not the audio. `Ok(None)` if the file has no (or an empty) model tag.
pub fn read_model_name(path: &Path) -> Result<Option<String>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    // The metadata blocks are tiny (STREAMINFO + Vorbis comment); read until the last one.
    let mut buf = Vec::new();
    let mut want = 4 + 4;
    loop {
        if buf.len() < want {
            let n = (&mut f).take((want - buf.len()) as u64).read_to_end(&mut buf)?;
            if n == 0 || buf.len() < want {
                bail!("truncated FLAC metadata in {}", path.display());
            }
        }
        if &buf[..4] != b"fLaC" {
            bail!("not a FLAC file: {}", path.display());
        }
        // Walk the block headers present so far.
        let mut p = 4;
        let mut complete = false;
        while p + 4 <= buf.len() {
            let len = u32::from_be_bytes([0, buf[p + 1], buf[p + 2], buf[p + 3]]) as usize;
            let last = buf[p] & 0x80 != 0;
            p += 4 + len;
            if last {
                complete = true;
                break;
            }
        }
        if complete && p <= buf.len() {
            break;
        }
        want = if complete { p } else { p + 4 }.max(buf.len() + 1);
    }
    let tags = read_tags(&buf)?;
    Ok(tags.into_iter().find(|(k, _)| k == "CLUBCRAFT_MODEL").map(|(_, v)| v).filter(|v| !v.is_empty()))
}

/// Read the Vorbis comments of a FLAC file (only the metadata header is parsed).
fn read_tags(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    if bytes.len() < 4 || &bytes[..4] != b"fLaC" {
        bail!("not a FLAC file");
    }
    let mut p = 4;
    let mut out = Vec::new();
    loop {
        let h = bytes.get(p..p + 4).ok_or_else(|| anyhow!("truncated FLAC header"))?;
        let last = h[0] & 0x80 != 0;
        let typ = h[0] & 0x7f;
        let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as usize;
        let body = bytes.get(p + 4..p + 4 + len).ok_or_else(|| anyhow!("truncated FLAC metadata"))?;
        if typ == 4 {
            let rd = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(body.get(o..o + 4)?.try_into().ok()?)) };
            let mut o = 4 + rd(0).unwrap_or(0) as usize;
            let n = rd(o).unwrap_or(0);
            o += 4;
            for _ in 0..n {
                let Some(l) = rd(o) else { break };
                let Some(s) = body.get(o + 4..o + 4 + l as usize) else { break };
                let s = String::from_utf8_lossy(s);
                if let Some((k, v)) = s.split_once('=') {
                    out.push((k.to_ascii_uppercase(), v.to_string()));
                }
                o += 4 + l as usize;
            }
        }
        p += 4 + len;
        if last {
            break;
        }
    }
    Ok(out)
}

/// `fill(a, b, k, out)`: append quantised interleaved samples of frames `a..b`.
type FillFn<'a> = dyn Fn(usize, usize, f32, &mut Vec<i32>) + 'a;

/// Streams quantised samples to the encoder so no full-track integer copy is needed.
struct StemSource<'a> {
    frames: usize,
    pos: usize,
    k: f32,
    fill: &'a FillFn<'a>,
    buf: Vec<i32>,
}

impl flacenc::source::Source for StemSource<'_> {
    fn channels(&self) -> usize {
        STEM_CHANNELS
    }
    fn bits_per_sample(&self) -> usize {
        16
    }
    fn sample_rate(&self) -> usize {
        SAMPLE_RATE as usize
    }
    fn read_samples<F: flacenc::source::Fill>(
        &mut self,
        block_size: usize,
        dest: &mut F,
    ) -> Result<usize, flacenc::error::SourceError> {
        let end = (self.pos + block_size).min(self.frames);
        self.buf.clear();
        (self.fill)(self.pos, end, self.k, &mut self.buf);
        dest.fill_interleaved(&self.buf)?;
        let n = end - self.pos;
        self.pos = end;
        Ok(n)
    }
    fn len_hint(&self) -> Option<usize> {
        Some(self.frames)
    }
}

/// Encode 8-channel frames as FLAC. `fill(a, b, k, out)` appends the interleaved samples of frames
/// `a..b`, quantised with factor `k` (see [`q`]).
fn write_flac(
    path: &Path,
    frames: usize,
    model: &str,
    peak: f32,
    fill: &FillFn,
) -> Result<()> {
    let scale = (peak * (32768.0 / 32767.0)).max(1.0);
    let mut config = flacenc::config::Encoder::default();
    config.block_size = BLOCK;
    let config = config.into_verified().map_err(|e| anyhow!("flac config: {e:?}"))?;
    let source = StemSource { frames, pos: 0, k: 32768.0 / scale, fill, buf: Vec::new() };
    let mut stream =
        flacenc::encode_with_fixed_block_size(&config, source, BLOCK).map_err(|e| anyhow!("flac encode: {e:?}"))?;
    let tags = vorbis_comment(&[
        ("CLUBCRAFT_STEMS", "drums,bass,other,vocals".into()),
        ("CLUBCRAFT_MODEL", model.to_string()),
        ("CLUBCRAFT_SCALE", format!("{scale}")),
    ]);
    stream.add_metadata_block(MetadataBlockData::new_unknown(4, &tags).map_err(|e| anyhow!("{e:?}"))?);
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|e| anyhow!("flac write: {e:?}"))?;
    let mut bytes = sink.into_inner();
    // flacenc records the (shorter) final block as the STREAMINFO minimum block size; the FLAC spec
    // excludes the last block, and symphonia treats min != max as a variable-block-size stream and
    // then fails to sync. Fix it up: the stream is fixed-blocksize.
    // (Like the reference encoder, a single short frame also gets min = max = BLOCK.)
    if bytes.len() > 12 && &bytes[..4] == b"fLaC" && bytes[4] & 0x7f == 0 {
        bytes[8..10].copy_from_slice(&(BLOCK as u16).to_be_bytes());
        bytes[10..12].copy_from_slice(&(BLOCK as u16).to_be_bytes());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    // Write atomically so a crash never leaves a truncated cache file behind.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[inline]
fn q(v: f32, k: f32) -> i32 {
    (v * k).round().clamp(-32768.0, 32767.0) as i32
}

/// Save separated stems (44.1 kHz) as an 8-channel FLAC cache file.
pub fn save_stems(path: &Path, stems: &Stems) -> Result<()> {
    save_stems_with_model(path, stems, "")
}

/// Like [`save_stems`], recording the model name (returned as [`StemTrack::model`] on load).
pub fn save_stems_with_model(path: &Path, stems: &Stems, model: &str) -> Result<()> {
    if stems.sample_rate != SAMPLE_RATE {
        bail!("stem cache must be {SAMPLE_RATE} Hz");
    }
    let n = stems.frames();
    let peak = stems.stems.iter().flat_map(|(l, r)| l.iter().chain(r)).fold(0f32, |m, v| m.max(v.abs()));
    write_flac(path, n, model, peak, &|a, b, k, out| {
        for f in a..b {
            for (l, r) in &stems.stems {
                out.push(q(l[f], k));
                out.push(q(r[f], k));
            }
        }
    })
}

/// Save a (complete) [`StemTrack`]; missing chunks are written as silence.
pub fn save_stem_track(path: &Path, t: &StemTrack) -> Result<()> {
    let peak = (0..t.num_chunks())
        .filter_map(|c| t.chunk(c))
        .flat_map(|c| c.data.iter())
        .fold(0f32, |m, v| m.max(v.abs()));
    write_flac(path, t.frames, &t.model, peak, &|a, b, k, out| {
        // Blocks never straddle chunks: BLOCK divides STEM_CHUNK_FRAMES.
        let c = a / STEM_CHUNK_FRAMES;
        let o = (a - c * STEM_CHUNK_FRAMES) * STEM_CHANNELS;
        let n = (b - a) * STEM_CHANNELS;
        match t.chunk(c) {
            Some(ch) => out.extend(ch.data[o..o + n].iter().map(|v| q(*v, k))),
            None => out.resize(out.len() + n, 0),
        }
    })
}

/// Decode a stem cache file into interleaved 8-channel chunks of `STEM_CHUNK_FRAMES` frames
/// (already scaled). FLAC frames are independent, so chunks are decoded on several threads.
/// Returns (frames, model, chunks).
fn decode(path: &Path) -> Result<(usize, String, Vec<Vec<f32>>)> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let tags = read_tags(&bytes)?;
    let tag = |k: &str| tags.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
    if tag("CLUBCRAFT_STEMS").is_none() {
        bail!("{} is not a ClubCraft stem file", path.display());
    }
    let scale: f32 = tag("CLUBCRAFT_SCALE").and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let model = tag("CLUBCRAFT_MODEL").unwrap_or_default();
    // STREAMINFO (always the first block): block sizes and total sample count.
    let si = bytes.get(8..26).ok_or_else(|| anyhow!("truncated STREAMINFO"))?;
    let (min_bs, max_bs) = (u16::from_be_bytes([si[0], si[1]]) as usize, u16::from_be_bytes([si[2], si[3]]) as usize);
    let total = (u64::from_be_bytes(si[10..18].try_into()?) & ((1u64 << 36) - 1)) as usize;
    let per_chunk = if total <= max_bs.max(1) {
        1 // a single FLAC frame
    } else if min_bs == max_bs && STEM_CHUNK_FRAMES.is_multiple_of(max_bs) {
        STEM_CHUNK_FRAMES / max_bs
    } else {
        bail!("unsupported stem file block size {min_bs}..{max_bs}");
    };

    let mss = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("flac");
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .context("probe")?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| anyhow!("no audio track"))?;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or_else(|| anyhow!("no audio params"))?.clone();
    let track_id = track.id;
    let mut packets = Vec::new();
    loop {
        match format.next_packet() {
            Ok(Some(p)) if p.track_id == track_id => packets.push(p),
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(anyhow::Error::from(e).context(format!("packet {}", packets.len()))),
        }
    }
    drop(format);
    let nchunks = total.div_ceil(STEM_CHUNK_FRAMES);
    let expect = if total <= max_bs.max(1) { usize::from(total > 0) } else { total.div_ceil(max_bs) };
    if packets.len() != expect {
        bail!("stem file has {} FLAC frames, expected {expect}", packets.len());
    }
    let chunk_len = |c: usize| (total - c * STEM_CHUNK_FRAMES).min(STEM_CHUNK_FRAMES) * STEM_CHANNELS;
    let mut chunks: Vec<Vec<f32>> = (0..nchunks).map(|_| Vec::new()).collect();
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8).min(nchunks.max(1));
    let per_worker = nchunks.div_ceil(workers.max(1)).max(1);
    let packets = &packets;
    let params = &params;
    std::thread::scope(|s| -> Result<()> {
        let mut handles = Vec::new();
        for (w, group) in chunks.chunks_mut(per_worker).enumerate() {
            handles.push(s.spawn(move || -> Result<()> {
                let mut decoder =
                    symphonia::default::get_codecs().make_audio_decoder(params, &AudioDecoderOptions::default())?;
                for (gi, out) in group.iter_mut().enumerate() {
                    let c = w * per_worker + gi;
                    let want = chunk_len(c);
                    out.reserve_exact(want);
                    for pk in &packets[c * per_chunk..((c + 1) * per_chunk).min(packets.len())] {
                        let audio = decoder.decode(pk).context("decode stem file")?;
                        if audio.spec().channels().count() != STEM_CHANNELS {
                            bail!("stem file must have {STEM_CHANNELS} channels");
                        }
                        let o = out.len();
                        out.resize(o + audio.samples_interleaved(), 0.0);
                        audio.copy_to_slice_interleaved(&mut out[o..]);
                    }
                    if out.len() != want {
                        bail!("stem file chunk {c}: {} samples, expected {want}", out.len());
                    }
                    if scale != 1.0 {
                        out.iter_mut().for_each(|v| *v *= scale);
                    }
                }
                Ok(())
            }));
        }
        for h in handles {
            h.join().map_err(|_| anyhow!("decoder thread panicked"))??;
        }
        Ok(())
    })?;
    Ok((total, model, chunks))
}

/// Load a stem cache file into planar [`Stems`].
pub fn load_stems(path: &Path) -> Result<Stems> {
    let (frames, _, chunks) = decode(path)?;
    let mut stems: [(Vec<f32>, Vec<f32>); 4] = Default::default();
    std::thread::scope(|s| {
        for (k, (l, r)) in stems.iter_mut().enumerate() {
            let chunks = &chunks;
            s.spawn(move || {
                l.reserve_exact(frames);
                r.reserve_exact(frames);
                for c in chunks {
                    for f in c.chunks_exact(STEM_CHANNELS) {
                        l.push(f[2 * k]);
                        r.push(f[2 * k + 1]);
                    }
                }
            });
        }
    });
    Ok(Stems { sample_rate: SAMPLE_RATE, stems })
}

/// Load a stem cache file as a complete [`StemTrack`] (for the live engine).
pub fn load_stem_track(path: &Path) -> Result<StemTrack> {
    let (frames, model, chunks) = decode(path)?;
    let t = StemTrack::new(frames, model);
    for (i, c) in chunks.into_iter().enumerate() {
        t.set_chunk(i, c);
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let n = STEM_CHUNK_FRAMES + 12345;
        let mut st = Stems { sample_rate: SAMPLE_RATE, ..Default::default() };
        for (s, (l, r)) in st.stems.iter_mut().enumerate() {
            *l = (0..n).map(|i| ((i as f32) * 0.001 * (s + 1) as f32).sin() * 0.8).collect();
            *r = (0..n).map(|i| ((i as f32) * 0.0013 * (s + 1) as f32).cos() * 1.7).collect(); // > 0 dBFS
        }
        let dir = std::env::var("CCSTEMS_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join(format!("ccstems-{}", std::process::id())));
        let p = dir.join("t.stems.flac");
        save_stems_with_model(&p, &st, "htdemucs").unwrap();
        let back = match load_stems(&p) {
            Ok(b) => b,
            Err(e) => panic!("{e:?}"),
        };
        for (a, b) in st.stems.iter().zip(&back.stems) {
            assert_eq!(a.0.len(), b.0.len());
            let err = a.0.iter().zip(&b.0).chain(a.1.iter().zip(&b.1)).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
            assert!(err < 1.7 / 32768.0, "err {err}");
        }
        let t = load_stem_track(&p).unwrap();
        assert!(t.is_complete());
        assert_eq!(t.model, "htdemucs");
        assert_eq!(read_model_name(&p).unwrap().as_deref(), Some("htdemucs"));
        assert_eq!(t.frames, n);
        let f = t.frame(STEM_CHUNK_FRAMES + 7).unwrap();
        assert!((f[3] - st.stems[1].1[STEM_CHUNK_FRAMES + 7]).abs() < 1e-4);
        save_stem_track(&p, &t).unwrap();
        let t2 = load_stem_track(&p).unwrap();
        assert_eq!(t2.chunk(1).unwrap().data, t.chunk(1).unwrap().data);
        // Short file (single FLAC frame).
        let short = Stems {
            sample_rate: SAMPLE_RATE,
            stems: std::array::from_fn(|s| (vec![0.1 * s as f32; 1000], vec![-0.2; 1000])),
        };
        save_stems(&p, &short).unwrap();
        let back = load_stems(&p).unwrap();
        assert_eq!(back.frames(), 1000);
        assert!((back.stems[3].0[999] - 0.3).abs() < 1e-4);
        assert_eq!(read_model_name(&p).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
