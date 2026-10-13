//! Offline Studio export: renders a [`Mix`] through the same decks, mixer and conductor as the
//! live engine, as fast as the CPU allows.

use crate::command::Command;
use crate::conductor::MixSchedule;
use crate::core::EngineCore;
use crate::track::LoadedTrack;
use crate::wav::{WavFormat, WavSink};
use anyhow::Result;
use clubcraft_core::{Mix, MixTimeline, TrackId};
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

/// Render `mix` to a WAV file. `progress(fraction)` is called regularly; return `false` to
/// cancel (the partial file is kept and the report says how much was rendered).
pub fn render_mix(
    mix: &Mix,
    tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>,
    out: &Path,
    opts: &RenderOptions,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<RenderReport> {
    let mut sink = WavSink::create(out, opts.sample_rate, opts.format)?;
    let mut err = None;
    let report = render_with(mix, tracks, opts, progress, &mut |buf| {
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

/// Render to memory (interleaved stereo). Handy for previews and tests.
pub fn render_mix_to_vec(mix: &Mix, tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>, opts: &RenderOptions) -> Result<(Vec<f32>, RenderReport)> {
    let mut v = Vec::new();
    let r = render_with(mix, tracks, opts, &mut |_| true, &mut |b| v.extend_from_slice(b))?;
    Ok((v, r))
}

fn render_with(
    mix: &Mix,
    tracks: &dyn Fn(TrackId) -> Option<Arc<LoadedTrack>>,
    opts: &RenderOptions,
    progress: &mut dyn FnMut(f32) -> bool,
    out: &mut dyn FnMut(&[f32]),
) -> Result<RenderReport> {
    let schedule = MixSchedule::new(mix, tracks, [0, 1])?;
    let timeline = schedule.timeline.clone();
    let total = timeline.total_secs;
    let mut core = EngineCore::new(opts.sample_rate, 2);
    core.apply(Command::SetLimiter { enabled: opts.limiter, ceiling_db: opts.ceiling_db });
    core.apply(Command::SetMasterGain(opts.master_gain_db));
    core.apply(Command::PlayMix { schedule: Box::new(schedule), start: 0.0 });
    let chunk = 4096usize;
    let mut master = vec![0f32; chunk * 2];
    let mut cue = vec![0f32; chunk * 2];
    let mut frames = 0u64;
    let mut peak = 0f32;
    let t0 = Instant::now();
    let tail_frames = (opts.tail_secs * opts.sample_rate as f64) as u64;
    let mut tail_left: Option<u64> = None;
    let mut last_report = 0u64;
    loop {
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
    let secs = frames as f64 / opts.sample_rate as f64;
    Ok(RenderReport { frames, seconds: secs, peak, speed: secs / t0.elapsed().as_secs_f64().max(1e-9), timeline })
}
