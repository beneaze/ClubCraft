//! Offline Studio export: renders a [`Mix`] through the same decks, mixer and conductor as the
//! live engine, as fast as the CPU allows.

use crate::command::Command;
use crate::conductor::MixSchedule;
use crate::core::EngineCore;
use crate::track::LoadedTrack;
use crate::wav::{WavFormat, WavSink};
use anyhow::{anyhow, bail, Result};
use clubcraft_core::{Analysis, Mix, MixTimeline, TrackId};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

/// Render settings.
#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub sample_rate: u32,
    pub format: WavFormat,
    /// Master limiter (recommended: renders are loudness-normalised per track).
    pub limiter: bool,
    pub ceiling_db: f32,
    /// Extra time rendered after the end of the mix for echo / reverb tails.
    pub tail_secs: f64,
    pub master_gain_db: f32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { sample_rate: 44100, format: WavFormat::Pcm24, limiter: true, ceiling_db: -1.0, tail_secs: 2.0, master_gain_db: 0.0 }
    }
}

/// Result of a render.
#[derive(Clone, Debug)]
pub struct RenderReport {
    pub frames: u64,
    pub seconds: f64,
    /// Highest sample peak (linear).
    pub peak: f32,
    /// Rendered audio seconds per wall-clock second.
    pub speed: f64,
    pub timeline: MixTimeline,
}

/// Render `mix` to a WAV file.
///
/// * `analyses` gives each track's metadata (grid, duration, cues, gain) for the timeline;
///   it is called once per track up front and must be cheap (no audio).
/// * `loader` returns the audio + stems; it is called **just before** a track starts (in mix
///   order, once per track), and each track is dropped as soon as its part of the mix has
///   passed, so at most 2–3 tracks are resident at any time.
///
/// `progress(fraction)` is called regularly; return `false` to cancel (the partial file is kept
/// and the report says how much was rendered).
pub fn render_mix(
    mix: &Mix,
    analyses: &dyn Fn(TrackId) -> Option<Analysis>,
    loader: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>,
    out: &Path,
    opts: &RenderOptions,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<RenderReport> {
    let mut sink = WavSink::create(out, opts.sample_rate, opts.format)?;
    let mut err = None;
    let report = render_with(mix, analyses, loader, opts, progress, &mut |buf| {
        if err.is_none() {
            if let Err(e) = sink.write(buf) {
                err = Some(e);
            }
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    sink.finish()?;
    Ok(report)
}

/// Render to memory (interleaved stereo). Handy for previews and tests. Same loading rules as
/// [`render_mix`].
pub fn render_mix_to_vec(
    mix: &Mix,
    analyses: &dyn Fn(TrackId) -> Option<Analysis>,
    loader: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>,
    opts: &RenderOptions,
) -> Result<(Vec<f32>, RenderReport)> {
    let mut v = Vec::new();
    let r = render_with(mix, analyses, loader, opts, &mut |_| true, &mut |b| v.extend_from_slice(b))?;
    Ok((v, r))
}

fn render_with(
    mix: &Mix,
    analyses: &dyn Fn(TrackId) -> Option<Analysis>,
    loader: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>,
    opts: &RenderOptions,
    progress: &mut dyn FnMut(f32) -> bool,
    out: &mut dyn FnMut(&[f32]),
) -> Result<RenderReport> {
    let schedule = MixSchedule::lazy(mix, analyses, [0, 1]);
    if schedule.is_empty() {
        bail!("empty mix");
    }
    let id = schedule.id;
    let ids = schedule.track_ids.clone();
    let timeline = schedule.timeline.clone();
    let total = timeline.total_secs;
    let sr = opts.sample_rate as f64;
    let mut core = EngineCore::new(opts.sample_rate, 2);
    core.apply(Command::SetLimiter { enabled: opts.limiter, ceiling_db: opts.ceiling_db });
    core.apply(Command::SetMasterGain(opts.master_gain_db));
    core.apply(Command::PlayMix { schedule: Box::new(schedule), start: 0.0 });
    let chunk = 4096usize;
    let mut master = vec![0f32; chunk * 2];
    let mut cue = vec![0f32; chunk * 2];
    let mut frames = 0u64;
    let mut peak = 0f32;
    let mut loaded = vec![false; ids.len()];
    let t0 = Instant::now();
    let tail_frames = (opts.tail_secs * sr) as u64;
    let mut tail_left: Option<u64> = None;
    let mut last_report = 0u64;
    loop {
        // just-in-time loading: everything that starts before the end of this chunk
        let t = core.mix_time();
        let horizon = t + chunk as f64 / sr + 0.05;
        for (i, tr) in timeline.tracks.iter().enumerate() {
            if !loaded[i] && tr.start_sec <= horizon && tr.end_sec > t {
                let track = loader(ids[i]).ok_or_else(|| anyhow!("track {} not available", ids[i]))?;
                core.apply(Command::MixTrackReady { schedule: id, index: i, track });
                loaded[i] = true;
            }
        }
        core.process(&mut master, &mut cue);
        for s in &master {
            peak = peak.max(s.abs());
        }
        out(&master);
        frames += chunk as u64;
        if core.mix_finished() && tail_left.is_none() {
            tail_left = Some(tail_frames);
        }
        if let Some(t) = &mut tail_left {
            if *t <= chunk as u64 {
                break;
            }
            *t -= chunk as u64;
        }
        if frames - last_report >= opts.sample_rate as u64 {
            last_report = frames;
            if !progress((core.mix_time() / total.max(1e-9)).min(1.0) as f32) {
                break;
            }
        }
    }
    progress(1.0);
    let secs = frames as f64 / sr;
    Ok(RenderReport { frames, seconds: secs, peak, speed: secs / t0.elapsed().as_secs_f64().max(1e-9), timeline })
}
