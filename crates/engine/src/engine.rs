//! Real-time runtime: opens a cpal output stream and runs an [`EngineCore`] in its callback.
//!
//! * Commands: lock-free SPSC ring (rtrb) UI → audio. [`EngineHandle::send`] may be called from
//!   any thread (a mutex serialises *senders*; the audio thread never locks).
//! * State: the audio thread publishes an [`EngineState`] snapshot into a triple buffer after
//!   each callback; [`EngineHandle::state`] returns the latest copy.
//! * Garbage: tracks / schedules / recorder rings replaced on the audio thread are pushed to a
//!   ring that a collector thread drains and drops.
//! * Headphones: channel pairs, mono split, or a second stereo device with drift correction.

use crate::command::Command;
use crate::core::{EngineCore, Garbage};
use crate::output::{self, CueFrame, CueOutput, Delay, OutputHealth};
use crate::state::EngineState;
use crate::wav::{RecorderThread, WavFormat};
use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Where the headphone (cue) bus goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HeadphoneRouting {
    /// Independent cue/preview volume summed into the physical master output.
    MainOutput,
    /// Channels 3/4 when the device has ≥ 4 outputs, otherwise no headphone output.
    #[default]
    Auto,
    /// Master only.
    Off,
    /// Explicit 0-based channel pairs.
    Channels { master: [u16; 2], cue: [u16; 2] },
    /// Stereo device with a splitter cable: mono master on the left, mono cue on the right.
    Split,
    /// Stereo cue on a second output device; master remains on channels 1/2.
    SeparateDevice,
}

/// Audio output configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Output device ID (legacy names are accepted); `None` = system default.
    pub device: Option<String>,
    /// Preferred rate for one device; required for SeparateDevice.
    /// `None` = 44.1 kHz if supported, else 48 kHz (then device default for one device).
    pub sample_rate: Option<u32>,
    /// Requested buffer size in frames (`None` = device default). 256 ≈ 5.8 ms at 44.1 kHz.
    pub buffer_frames: Option<u32>,
    pub headphones: HeadphoneRouting,
    #[serde(default)]
    pub headphone_device: Option<String>,
    #[serde(default = "stereo_pair")]
    pub master_pair: [u16; 2],
    #[serde(default = "stereo_pair")]
    pub headphone_pair: [u16; 2],
    /// Positive values mean the headphones lag further: delay the master.
    #[serde(default)]
    pub alignment_ms: f64,
    /// Binds the saved correction to resolved devices, rate and buffer.
    #[serde(default)]
    pub alignment_key: Option<String>,
    /// 2 or 4 decks.
    pub decks: usize,
}

fn stereo_pair() -> [u16; 2] { [0, 1] }

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig { device: None, sample_rate: None, buffer_frames: Some(256), headphones: HeadphoneRouting::MainOutput,
            headphone_device: None, master_pair: [0, 1], headphone_pair: [0, 1], alignment_ms: 0.0, alignment_key: None, decks: 4 }
    }
}

/// An output device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub id: String,
    /// Maximum number of output channels.
    pub max_channels: u16,
    /// Supported rates among 44.1 / 48 / 88.2 / 96 kHz.
    pub sample_rates: Vec<u32>,
    pub is_default: bool,
}

/// What was actually opened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: Option<u32>,
    /// Output channel pairs; cue refers to `headphones` when a second device is open.
    /// `None` means no headphone output.
    pub master_channels: [u16; 2],
    pub cue_channels: Option<[u16; 2]>,
    pub split: bool,
    pub master: EndpointInfo,
    pub headphones: Option<EndpointInfo>,
    pub warning: Option<String>,
    pub cue_underruns: u32,
    pub cue_overflows: u32,
    pub estimated_offset_ms: Option<f64>,
    pub master_delay_ms: f64,
    pub cue_delay_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EndpointInfo {
    pub device: String,
    pub id: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub sample_format: String,
    pub requested_buffer_frames: Option<u32>,
    pub observed_buffer_frames: Option<u32>,
    pub estimated_latency_ms: Option<f64>,
    pub failed: bool,
    pub errors: u32,
}

fn device_name(d: &cpal::Device) -> String {
    d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| "Unknown device".into())
}

/// List output devices.
pub fn devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default = host.default_output_device().and_then(|d| d.id().ok()).map(|id| id.to_string());
    let mut out = Vec::new();
    for d in host.output_devices()? {
        let name = device_name(&d);
        let id = d.id().map(|i| i.to_string()).unwrap_or_default();
        let mut max_ch = 0u16;
        let mut rates = Vec::new();
        if let Ok(cfgs) = d.supported_output_configs() {
            for c in cfgs.filter(|c| c.sample_format() == cpal::SampleFormat::F32) {
                max_ch = max_ch.max(c.channels());
                for r in [44100u32, 48000, 88200, 96000] {
                    if c.contains_rate(r) && !rates.contains(&r) {
                        rates.push(r);
                    }
                }
            }
        }
        rates.sort();
        if max_ch == 0 {
            continue;
        }
        out.push(DeviceInfo { is_default: Some(&id) == default.as_ref(), name, id, max_channels: max_ch, sample_rates: rates });
    }
    Ok(out)
}

/// Clonable, thread-safe command sender (for MIDI threads etc.).
#[derive(Clone)]
pub struct CommandSender {
    inner: Arc<Mutex<rtrb::Producer<Command>>>,
}

impl CommandSender {
    /// Queue a command. Fails only when the queue (4096 commands) is full.
    pub fn send(&self, cmd: Command) -> Result<()> {
        self.inner.lock().push(cmd).map_err(|_| anyhow!("engine command queue full"))
    }
}

struct AudioSide {
    core: Box<EngineCore>,
    cmds: rtrb::Consumer<Command>,
    master: Vec<f32>,
    cue: Vec<f32>,
}

const SCRATCH_FRAMES: usize = 4096;

/// The running engine.
pub struct EngineHandle {
    sender: CommandSender,
    state: Mutex<triple_buffer::Output<EngineState>>,
    runtime: Mutex<Runtime>,
    config: Mutex<AudioConfig>,
    info: Mutex<StreamInfo>,
    errors: Arc<AtomicU32>,
    collector_stop: Arc<AtomicBool>,
    collector: Option<std::thread::JoinHandle<()>>,
    recorder: Mutex<Option<RecorderThread>>,
}

/// The core is parked here whenever no device can be opened. Stream destruction
/// returns its loan through a ring, including when a disconnected device stops callbacks.
struct Runtime {
    streams: Option<Streams>,
    parked: Option<Box<AudioSide>>,
    returns: rtrb::Consumer<Box<AudioSide>>,
    health: Arc<OutputHealth>,
}

struct Streams {
    master: cpal::Stream,
    cue: Option<cpal::Stream>,
    stop: Arc<AtomicBool>,
}

impl Drop for Streams {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.master.pause();
        if let Some(cue) = &self.cue { let _ = cue.pause(); }
    }
}

struct CoreLoan {
    side: Option<Box<AudioSide>>,
    incoming: rtrb::Consumer<Box<AudioSide>>,
    returns: rtrb::Producer<Box<AudioSide>>,
}

impl CoreLoan {
    fn get(&mut self) -> Option<&mut AudioSide> {
        if self.side.is_none() { self.side = self.incoming.pop().ok(); }
        self.side.as_deref_mut()
    }
}

impl Runtime {
    fn reclaim(&mut self) -> Result<()> {
        drop(self.streams.take());
        if self.parked.is_none() {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                if let Ok(side) = self.returns.pop() { self.parked = Some(side); break; }
                if Instant::now() >= deadline { bail!("audio stream has not released the core yet; retry Apply"); }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }

    fn open(&mut self, config: &AudioConfig, prepared: Prepared, errors: Arc<AtomicU32>) -> Result<StreamInfo> {
        if self.parked.is_none() { bail!("audio core is still returning; retry Apply"); }
        let (rp, rc) = rtrb::RingBuffer::new(2);
        let health = Arc::new(OutputHealth::default());
        health.offset_us.store(i32::MIN, Ordering::Relaxed);
        let (streams, info) = open_stream(config, prepared, &mut self.parked, rp, health.clone(), errors)?;
        self.returns = rc;
        self.health = health;
        self.streams = Some(streams);
        Ok(info)
    }
}

impl Drop for CoreLoan {
    fn drop(&mut self) {
        // Exactly one core is lent and the return ring has capacity two.
        if let Some(side) = self.side.take().or_else(|| self.incoming.pop().ok()) {
            let _ = self.returns.push(side);
        }
    }
}

/// Entry point.
pub struct Engine;

impl Engine {
    /// Open the output device and start the engine.
    pub fn start(config: AudioConfig) -> Result<EngineHandle> {
        Self::start_with_commands(config, std::iter::empty())
    }

    /// Apply initial settings before starting either output callback. Diagnostic
    /// tools can set minimum gains without racing a running audio stream.
    pub fn start_with_commands(mut config: AudioConfig, commands: impl IntoIterator<Item = Command>) -> Result<EngineHandle> {
        let prepared = prepare(&config);
        let (prepared, warning) = match prepared {
            Ok(p) => (p, None),
            Err(e) if config.headphones == HeadphoneRouting::SeparateDevice => {
                let mut master_only = config.clone();
                master_only.headphones = HeadphoneRouting::Off;
                (prepare(&master_only)?, Some(format!("Headphones unavailable: {e:#}. Master only; select devices and Apply to retry.")))
            }
            Err(e) => return Err(e),
        };
        if warning.is_none() { bind_config(&mut config, &prepared); }
        let (gp, mut gc) = rtrb::RingBuffer::<Garbage>::new(1024);
        let (cp, cc) = rtrb::RingBuffer::<Command>::new(4096);
        let (_, rc) = rtrb::RingBuffer::<Box<AudioSide>>::new(2);
        let (sin, sout) = triple_buffer::triple_buffer(&EngineState::default());
        let collector_stop = Arc::new(AtomicBool::new(false));
        let stop = collector_stop.clone();
        let mut core = Box::new(EngineCore::new(prepared.sr, config.decks));
        core.set_garbage(gp);
        core.set_state_output(sin);
        for command in commands { core.apply(command); }
        let side = Box::new(AudioSide { core, cmds: cc, master: vec![0.0; SCRATCH_FRAMES * 2], cue: vec![0.0; SCRATCH_FRAMES * 2] });
        let errors = Arc::new(AtomicU32::new(0));
        let health = Arc::new(OutputHealth::default());
        let mut runtime = Runtime { streams: None, parked: Some(side), returns: rc, health };
        let info = match runtime.open(&config, prepared, errors.clone()) {
            Ok(mut info) => { info.warning = warning; info }
            Err(e) if config.headphones == HeadphoneRouting::SeparateDevice => {
                let mut master_only = config.clone();
                master_only.headphones = HeadphoneRouting::Off;
                let mut info = runtime.open(&master_only, prepare(&master_only)?, errors.clone())?;
                info.warning = Some(format!("Headphones unavailable: {e:#}. Master only; Apply to retry."));
                info
            }
            Err(e) => return Err(e),
        };
        let collector_result = std::thread::Builder::new()
            .name("clubcraft-gc".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    while let Ok(g) = gc.pop() {
                        drop(g);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                while let Ok(g) = gc.pop() {
                    drop(g);
                }
            })
            .context("spawn collector");
        let collector = collector_result?;
        Ok(EngineHandle {
            sender: CommandSender { inner: Arc::new(Mutex::new(cp)) },
            state: Mutex::new(sout),
            runtime: Mutex::new(runtime),
            config: Mutex::new(config),
            info: Mutex::new(info),
            errors,
            collector_stop,
            collector: Some(collector),
            recorder: Mutex::new(None),
        })
    }
}

fn find_device(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device> {
    if let Some(n) = name {
        let candidates: Vec<_> = host.output_devices()?.collect();
        let names: Vec<_> = candidates.iter().map(|d| (device_name(d), d.id().map(|id| id.to_string()).unwrap_or_default())).collect();
        let index = resolve_device(n, &names)?;
        return candidates.into_iter().nth(index).ok_or_else(|| anyhow!("audio device disappeared"));
    }
    host.default_output_device().ok_or_else(|| anyhow!("no audio output device"))
}

fn resolve_device(selected: &str, candidates: &[(String, String)]) -> Result<usize> {
    if let Some(i) = candidates.iter().position(|(_, id)| !id.is_empty() && id == selected) { return Ok(i); }
    let mut matches = candidates.iter().enumerate().filter(|(_, (name, _))| name == selected);
    let first = matches.next().map(|(i, _)| i).ok_or_else(|| anyhow!("audio device {selected:?} not found"))?;
    if matches.next().is_some() { bail!("multiple audio devices named {selected:?}; select a device again by ID"); }
    Ok(first)
}

struct Prepared {
    device: cpal::Device,
    cue_device: Option<cpal::Device>,
    channels: u16,
    sr: u32,
    master_ch: [u16; 2],
    cue_ch: Option<[u16; 2]>,
    split: bool,
}

fn bind_config(config: &mut AudioConfig, p: &Prepared) {
    let master = p.device.id().map(|i| i.to_string()).unwrap_or_default();
    let cue = p.cue_device.as_ref().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let key = format!("{:?}", (&master, &cue, p.sr, p.channels, config.buffer_frames, config.master_pair, config.headphone_pair));
    bind_alignment(config, key);
    // Keep None as the explicit system-default choice. Resolve legacy names once.
    if config.device.is_some() && !master.is_empty() { config.device = Some(master); }
    if cue.is_some() { config.headphone_device = cue; }
}

fn bind_alignment(config: &mut AudioConfig, key: String) {
    if config.alignment_key.as_ref().is_some_and(|old| old != &key) { config.alignment_ms = 0.0; }
    config.alignment_key = Some(key);
}

fn prepare(config: &AudioConfig) -> Result<Prepared> {
    if !config.alignment_ms.is_finite() || config.alignment_ms.abs() > 500.0 {
        bail!("headphone alignment must be between -500 and 500 ms");
    }
    if ![2, 4].contains(&config.decks) { bail!("select 2 or 4 decks"); }
    if config.buffer_frames == Some(0) { bail!("audio buffer must not be zero"); }
    let host = cpal::default_host();
    let device = find_device(&host, config.device.as_deref())?;
    let name = device_name(&device);
    let ranges: Vec<cpal::SupportedStreamConfigRange> =
        device.supported_output_configs()?.filter(|c| c.sample_format() == cpal::SampleFormat::F32).collect();
    if ranges.is_empty() {
        bail!("device {name} has no f32 output configuration");
    }
    let max_ch = ranges.iter().map(|r| r.channels()).max().unwrap_or(2);
    let (want_ch, master_ch, cue_ch, split) = match config.headphones {
        HeadphoneRouting::Auto if max_ch >= 4 => (4u16, [0u16, 1], Some([2u16, 3]), false),
        HeadphoneRouting::Auto => (2, [0, 1], None, false),
        HeadphoneRouting::MainOutput | HeadphoneRouting::Off | HeadphoneRouting::SeparateDevice => {
            let master = config.master_pair;
            if master[0] == master[1] { bail!("select distinct left and right master channels"); }
            let need = master.iter().copied().max().unwrap_or(1).checked_add(1).ok_or_else(|| anyhow!("invalid master channels"))?;
            if need > max_ch { bail!("master device has only {max_ch} outputs"); }
            (need.max(2), master, if config.headphones == HeadphoneRouting::MainOutput { Some(master) } else { None }, false)
        },
        HeadphoneRouting::Channels { master, cue } => {
            let need = master.iter().chain(cue.iter()).copied().max().unwrap_or(1).checked_add(1)
                .ok_or_else(|| anyhow!("invalid output channel"))?;
            if need > max_ch {
                bail!("device {name} has only {max_ch} outputs");
            }
            let mut used = [master[0], master[1], cue[0], cue[1]];
            used.sort();
            if used.windows(2).any(|w| w[0] == w[1]) { bail!("master and cue require distinct output channels"); }
            (need.max(2), master, Some(cue), false)
        }
        HeadphoneRouting::Split => (2, [0, 1], Some([0, 1]), true),
    };
    let cue_device = if config.headphones == HeadphoneRouting::SeparateDevice {
        let id = config.headphone_device.as_deref().ok_or_else(|| anyhow!("select a headphone output device"))?;
        let cue = find_device(&host, Some(id))?;
        if device.id()? == cue.id()? { bail!("select a different headphone device, or use single-device channel routing"); }
        Some(cue)
    } else { None };
    let cue_ranges = cue_device.as_ref().map(|d| d.supported_output_configs().map(|r| r.collect::<Vec<_>>())).transpose()?;
    let format = choose_format(&ranges, cue_ranges.as_deref(), config, want_ch);
    let (channels, sr) = match format {
        Ok(format) => format,
        Err(_) if config.headphones != HeadphoneRouting::SeparateDevice => {
            let default = device.default_output_config()?;
            default_format(&ranges, want_ch, default.sample_rate())?
        }
        Err(error) => return Err(error),
    };
    Ok(Prepared { device, cue_device, channels, sr, master_ch, cue_ch, split })
}

fn choose_format(ranges: &[cpal::SupportedStreamConfigRange], cue_ranges: Option<&[cpal::SupportedStreamConfigRange]>, config: &AudioConfig, want_ch: u16) -> Result<(u16, u32)> {
    let strict = config.headphones == HeadphoneRouting::SeparateDevice || cue_ranges.is_some();
    let master_ok = |c: &cpal::SupportedStreamConfigRange, sr| c.sample_format() == cpal::SampleFormat::F32
        && c.channels() >= want_ch && c.contains_rate(sr)
        && (!strict || supports_buffer(c, config.buffer_frames));
    let preferred: Vec<u32> = match config.sample_rate {
        Some(r) if strict => vec![r],
        Some(r) => vec![r, 44100, 48000],
        None => vec![44100, 48000],
    };
    let chosen = preferred
        .iter()
        .find(|r| ranges.iter().any(|c| master_ok(c, **r))
            && cue_ranges.is_none_or(|rs| rs.iter().any(|c| c.channels() >= config.headphone_pair.iter().copied().max().unwrap_or(1).saturating_add(1) && c.sample_format() == cpal::SampleFormat::F32
                && c.contains_rate(**r) && supports_buffer(c, config.buffer_frames))))
        .copied();
    let sr = chosen.ok_or_else(|| anyhow!("no common F32 output format for the selected rate, channels and buffer"))?;
    let channels = ranges.iter().filter(|c| master_ok(c, sr))
        .map(|c| c.channels()).min().ok_or_else(|| anyhow!("no supported output channel count"))?;
    Ok((channels, sr))
}

fn default_format(ranges: &[cpal::SupportedStreamConfigRange], want_ch: u16, rate: u32) -> Result<(u16, u32)> {
    let channels = ranges.iter().filter(|c| c.sample_format() == cpal::SampleFormat::F32
        && c.channels() >= want_ch && c.contains_rate(rate))
        .map(|c| c.channels()).min().ok_or_else(|| anyhow!("device default has no compatible F32 channel layout"))?;
    Ok((channels, rate))
}

fn supports_buffer(c: &cpal::SupportedStreamConfigRange, frames: Option<u32>) -> bool {
    match (c.buffer_size(), frames) {
        // WASAPI reports the shared engine period as min == max, not the
        // accepted request range. Let opening the stream validate the request;
        // its actual callback length is reported separately.
        (cpal::SupportedBufferSize::Range { min, max }, Some(_)) if cfg!(windows) && min == max => true,
        (cpal::SupportedBufferSize::Range { min, max }, Some(n)) => (*min..=*max).contains(&n),
        _ => true,
    }
}

fn endpoint(device: &cpal::Device, sr: u32, channels: u16, buffer: Option<u32>) -> EndpointInfo {
    EndpointInfo { device: device_name(device), id: device.id().map(|i| i.to_string()).unwrap_or_default(), sample_rate: sr, channels,
        sample_format: "F32".into(), requested_buffer_frames: buffer, observed_buffer_frames: None, estimated_latency_ms: None, failed: false, errors: 0 }
}

fn open_stream(
    config: &AudioConfig,
    prepared: Prepared,
    side: &mut Option<Box<AudioSide>>,
    ret: rtrb::Producer<Box<AudioSide>>,
    health: Arc<OutputHealth>,
    errors: Arc<AtomicU32>,
) -> Result<(Streams, StreamInfo)> {
    let Prepared { device, cue_device, channels, sr, master_ch, cue_ch, split } = prepared;
    let name = device_name(&device);
    let master_info = endpoint(&device, sr, channels, config.buffer_frames);
    let cue_channels = if let Some(d) = &cue_device {
        if config.headphone_pair[0] == config.headphone_pair[1] { bail!("select distinct left and right headphone channels"); }
        d.supported_output_configs()?.filter(|c| c.sample_format() == cpal::SampleFormat::F32 && c.contains_rate(sr)
            && c.channels() >= config.headphone_pair.iter().copied().max().unwrap_or(1).saturating_add(1)
            && supports_buffer(c, config.buffer_frames)).map(|c| c.channels()).min()
            .ok_or_else(|| anyhow!("headphone channels unavailable at the selected rate and buffer"))?
    } else { 2 };
    let cue_info = cue_device.as_ref().map(|d| endpoint(d, sr, cue_channels, config.buffer_frames));
    let buffer_size = match config.buffer_frames {
        Some(f) => cpal::BufferSize::Fixed(f),
        None => cpal::BufferSize::Default,
    };
    let stream_config = cpal::StreamConfig { channels, sample_rate: sr, buffer_size };
    let ch = channels as usize;
    let (mut lend, incoming) = rtrb::RingBuffer::new(1);
    let mut loan = CoreLoan { side: None, incoming, returns: ret };
    let epoch = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let (mut cue_tx, cue_rx) = rtrb::RingBuffer::<CueFrame>::new(sr as usize * 2);
    let mut delay = Delay::new(sr);
    let master_health = health.clone();
    let cue_stream = if let Some(d) = cue_device {
        let mut output = CueOutput::new(cue_rx, health.clone(), epoch, sr, config.alignment_ms);
        let err_health = health.clone();
        let cue_errors = errors.clone();
        let halted = stop.clone();
        let failed = health.clone();
        let cue_pair = config.headphone_pair;
        let stream = d.build_output_stream::<f32, _, _>(
            cpal::StreamConfig { channels: cue_channels, sample_rate: sr, buffer_size },
            move |data, info| {
                failed.cue.heartbeat(data.len() / cue_channels as usize);
                if halted.load(Ordering::Acquire) || failed.cue.failed.load(Ordering::Relaxed) || failed.master.failed.load(Ordering::Relaxed) {
                    data.fill(0.0);
                } else { output.render_channels(data, info, cue_channels as usize, cue_pair); }
            },
            move |error| {
                err_health.cue.report_error(error.kind());
                cue_errors.fetch_add(1, Ordering::Relaxed);
            }, Some(Duration::from_secs(3))).context("open headphone output")?;
        Some(stream)
    } else { None };
    let separate = cue_stream.is_some();
    let mut load = 0f32;
    let mut callback_health = crate::state::AudioHealth::default();
    let mut last_start: Option<Instant> = None;
    let mut window = (0f32, 0f32); // (peak in the current ~2 s window, seconds in it)
    let mc = master_ch;
    let shared = config.headphones == HeadphoneRouting::MainOutput;
    let cc = cue_ch;
    let mut shared_limiter = crate::dsp::Limiter::new(sr as f32);
    let mut shared_buffer = vec![0.0; SCRATCH_FRAMES * 2];
    let err2 = errors.clone();
    let data_cb = move |data: &mut [f32], callback: &cpal::OutputCallbackInfo| {
        let t0 = Instant::now();
        let faults0 = major_faults();
        master_health.master.heartbeat(data.len() / ch);
        if stopped.load(Ordering::Acquire) || master_health.master.failed.load(Ordering::Acquire) {
            data.fill(0.0);
            return;
        }
        let Some(side) = loan.get() else {
            data.fill(0.0);
            return;
        };
        for _ in 0..512 {
            match side.cmds.pop() {
                Ok(c) => side.core.apply(c),
                Err(_) => break,
            }
        }
        side.core.prof[0] = t0.elapsed().as_micros() as u32;
        let frames = data.len() / ch;
        let latency = output::observe(callback, frames, &master_health.master);
        let presentation = latency.map(|ns| t0.duration_since(epoch).as_nanos() as u64 + ns);
        let mut overflow = false;
        let master_delay = master_health.master_delay.load(Ordering::Relaxed) as f64;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(SCRATCH_FRAMES);
            let (m, c) = (&mut side.master[..n * 2], &mut side.cue[..n * 2]);
            side.core.process(m, c);
            if shared {
                output::mix_shared_output(m, c, &mut shared_buffer[..n * 2], &mut shared_limiter);
            }
            let out = &mut data[done * ch..(done + n) * ch];
            out.fill(0.0);
            for f in 0..n {
                let o = &mut out[f * ch..(f + 1) * ch];
                if separate && !master_health.cue.failed.load(Ordering::Relaxed) {
                    let frame = CueFrame { samples: [c[f * 2], c[f * 2 + 1]], master_ns: presentation
                        .map(|ns| ns + ((done + f) as u64 * 1_000_000_000 / sr as u64)).unwrap_or(0) };
                    if cue_tx.push(frame).is_err() { overflow = true; }
                }
                if split {
                    o[0] = 0.5 * (m[f * 2] + m[f * 2 + 1]);
                    o[1] = 0.5 * (c[f * 2] + c[f * 2 + 1]);
                } else {
                    let sample = if separate { delay.process([m[f * 2], m[f * 2 + 1]], master_delay) }
                        else { [m[f * 2], m[f * 2 + 1]] };
                    o[mc[0] as usize] = if shared { shared_buffer[f * 2] } else { sample[0] };
                    o[mc[1] as usize] = if shared { shared_buffer[f * 2 + 1] } else { sample[1] };
                    if let Some(cc) = cc.filter(|_| !shared) {
                        o[cc[0] as usize] = c[f * 2];
                        o[cc[1] as usize] = c[f * 2 + 1];
                    }
                }
            }
            done += n;
        }
        if overflow {
            master_health.overflows.fetch_add(1, Ordering::Relaxed);
            master_health.reset.store(true, Ordering::Relaxed);
        }
        let budget = frames as f32 / sr as f32;
        let l = t0.elapsed().as_secs_f32() / budget.max(1e-6);
        load = load * 0.9 + l * 0.1;
        side.core.cpu_load = load;
        // health: overruns, late callbacks, peaks
        callback_health.buffer_frames = frames as u32;
        if l > 1.0 {
            callback_health.overruns += 1;
        }
        if major_faults() > faults0 + 8 {
            callback_health.faulted += 1;
            if l > 1.0 {
                callback_health.faulted_overruns += 1;
            }
        }
        if let Some(prev) = last_start {
            if t0.duration_since(prev).as_secs_f32() > 2.0 * budget + 0.002 {
                callback_health.late += 1;
            }
        }
        last_start = Some(t0);
        callback_health.peak_load = callback_health.peak_load.max(l);
        window.0 = window.0.max(l);
        window.1 += budget;
        callback_health.recent_peak = callback_health.recent_peak.max(window.0);
        if window.1 >= 2.0 {
            callback_health.recent_peak = window.0;
            window = (0.0, 0.0);
        }
        let sections = side.core.audio.section_peak_us;
        for (k, v) in crate::prof::take().into_iter().enumerate() {
            callback_health.deck_peak_us[k] = callback_health.deck_peak_us[k].max(v);
        }
        side.core.audio = callback_health;
        side.core.audio.section_peak_us = sections;
        side.core.stream_errors = err2.load(Ordering::Relaxed);
    };
    let opening_health = health.clone();
    let err_cb = move |error: cpal::Error| {
        health.master.report_error(error.kind());
        errors.fetch_add(1, Ordering::Relaxed);
    };
    let stream = device
        .build_output_stream::<f32, _, _>(stream_config, data_cb, err_cb, Some(Duration::from_secs(3)))
        .with_context(|| format!("open {name} ({channels} ch @ {sr} Hz)"))?;
    // The callback owns no core until BOTH streams have opened and started.
    // An error before this point therefore leaves all deck state with the caller.
    if let Some(cue) = &cue_stream { cue.play().context("start headphone stream")?; }
    stream.play().context("start master stream")?;
    if opening_health.master.failed.load(Ordering::Acquire) || opening_health.cue.failed.load(Ordering::Acquire) {
        bail!("an audio stream failed while starting");
    }
    opening_health.master.arm();
    if separate { opening_health.cue.arm(); }
    let core = side.as_mut().ok_or_else(|| anyhow!("audio core has not returned from previous stream"))?;
    core.core.set_sample_rate(sr);
    core.core.set_shared_output(config.headphones == HeadphoneRouting::MainOutput);
    core.core.apply(Command::SetDeckCount(config.decks));
    if let Some(s) = side.take() {
        if let Err(rtrb::PushError::Full(s)) = lend.push(s) {
            *side = Some(s);
            bail!("audio core handoff failed");
        }
    }
    let info = StreamInfo {
        device: name,
        sample_rate: sr,
        channels,
        buffer_frames: config.buffer_frames,
        master_channels: master_ch,
        cue_channels: if separate { Some(config.headphone_pair) } else { cue_ch },
        split,
        master: master_info,
        headphones: cue_info,
        warning: None,
        cue_underruns: 0,
        cue_overflows: 0,
        estimated_offset_ms: None,
        master_delay_ms: 0.0,
        cue_delay_ms: 0.0,
    };
    Ok((Streams { master: stream, cue: cue_stream, stop }, info))
}

impl EngineHandle {
    /// Queue a command for the audio thread.
    pub fn send(&self, cmd: Command) -> Result<()> {
        self.sender.send(cmd)
    }

    /// A clonable sender (e.g. for the MIDI input thread).
    pub fn sender(&self) -> CommandSender {
        self.sender.clone()
    }

    /// Latest state snapshot (cheap copy; poll at UI rate).
    pub fn state(&self) -> EngineState {
        let mut state = *self.state.lock().read();
        state.stream_errors = self.errors.load(Ordering::Relaxed);
        state
    }

    pub fn stream_info(&self) -> StreamInfo {
        let rt = self.runtime.lock();
        let mut info = self.info.lock().clone();
        fn update(i: &mut EndpointInfo, h: &output::EndpointHealth) {
            h.log_errors(&i.device);
            i.observed_buffer_frames = match h.frames.load(Ordering::Relaxed) { 0 => None, n => Some(n) };
            i.estimated_latency_ms = match h.latency_us.load(Ordering::Relaxed) { 0 => None, n => Some(n as f64 / 1000.0) };
            i.failed = h.check_stalled(i.sample_rate);
            i.errors = h.errors.load(Ordering::Relaxed);
        }
        update(&mut info.master, &rt.health.master);
        if let Some(cue) = &mut info.headphones { update(cue, &rt.health.cue); }
        info.cue_underruns = rt.health.underruns.load(Ordering::Relaxed);
        info.cue_overflows = rt.health.overflows.load(Ordering::Relaxed);
        let offset = rt.health.offset_us.load(Ordering::Relaxed);
        info.estimated_offset_ms = (offset != i32::MIN && info.headphones.is_some()
            && info.master.estimated_latency_ms.is_some() && info.headphones.as_ref().is_some_and(|h| h.estimated_latency_ms.is_some()))
            .then_some(offset as f64 / 1000.0);
        info.master_delay_ms = rt.health.master_delay.load(Ordering::Relaxed) as f64 * 1000.0 / info.sample_rate as f64;
        info.cue_delay_ms = rt.health.cue_delay.load(Ordering::Relaxed) as f64 * 1000.0 / info.sample_rate as f64;
        if rt.streams.is_none() {
            info.master.failed = true;
            if let Some(cue) = &mut info.headphones { cue.failed = true; }
        }
        info
    }

    pub fn audio_config(&self) -> AudioConfig {
        self.config.lock().clone()
    }

    /// Output devices.
    pub fn devices(&self) -> Result<Vec<DeviceInfo>> {
        devices()
    }

    /// Reopen the output with a new configuration (device, rate, buffer, routing). Decks keep
    /// their tracks, positions and settings.
    pub fn set_audio_config(&self, mut config: AudioConfig) -> Result<()> {
        let prepared = prepare(&config)?;
        let mut rt = self.runtime.lock();
        if self.recorder.lock().is_some() && prepared.sr != self.info.lock().sample_rate {
            bail!("stop recording before changing the sample rate");
        }
        bind_config(&mut config, &prepared);
        let old_config = self.config.lock().clone();
        let old_info = self.info.lock().clone();
        let mut rollback = old_config.clone();
        rollback.device = Some(if old_info.master.id.is_empty() { old_info.master.device.clone() } else { old_info.master.id.clone() });
        rollback.sample_rate = Some(old_info.sample_rate);
        if old_config.headphones == HeadphoneRouting::SeparateDevice {
            rollback.headphones = if let Some(cue) = &old_info.headphones {
                rollback.headphone_device = Some(cue.id.clone());
                HeadphoneRouting::SeparateDevice
            } else { HeadphoneRouting::Off };
        }
        rt.reclaim()?;
        match rt.open(&config, prepared, self.errors.clone()) {
            Ok(info) => {
                *self.info.lock() = info;
                *self.config.lock() = config;
                Ok(())
            }
            Err(error) => {
                let restored = prepare(&rollback).and_then(|p| rt.open(&rollback, p, self.errors.clone()));
                match restored {
                    Ok(mut info) => {
                        info.warning = old_info.warning;
                        *self.info.lock() = info;
                        Err(error.context("could not change output; previous output restored"))
                    }
                    Err(restore) => {
                        self.info.lock().warning = Some(format!("Audio stopped; deck state retained. Apply to retry. {restore:#}"));
                        Err(error.context("could not change or restore output; deck state retained"))
                    }
                }
            }
        }
    }

    /// Start recording the master output to a WAV file.
    pub fn start_recording(&self, path: &Path, format: WavFormat) -> Result<()> {
        self.stop_recording()?;
        let rt = self.runtime.lock();
        let sr = self.info.lock().sample_rate;
        if rt.streams.is_none() || rt.health.master.check_stalled(sr) { bail!("master output is unavailable"); }
        let (p, c) = rtrb::RingBuffer::<f32>::new(sr as usize * 2 * 10);
        let t = RecorderThread::spawn(path.to_path_buf(), sr, format, c)?;
        self.send(Command::SetRecorder(Some(p)))?;
        *self.recorder.lock() = Some(t);
        Ok(())
    }

    /// Stop recording; returns the file path and number of frames written.
    pub fn stop_recording(&self) -> Result<Option<(PathBuf, u64)>> {
        let Some(t) = self.recorder.lock().take() else { return Ok(None) };
        if let Err(e) = self.send(Command::SetRecorder(None)) {
            *self.recorder.lock() = Some(t);
            return Err(e);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while !t.join.is_finished() {
            let mut rt = self.runtime.lock();
            let sr = self.info.lock().sample_rate;
            if rt.health.master.check_stalled(sr) || rt.streams.is_none() {
                if let Err(e) = rt.reclaim() {
                    *self.recorder.lock() = Some(t);
                    return Err(e);
                }
                if let Some(side) = &mut rt.parked {
                    while let Ok(cmd) = side.cmds.pop() { side.core.apply(cmd); }
                }
            }
            drop(rt);
            if Instant::now() >= deadline {
                *self.recorder.lock() = Some(t);
                bail!("recorder did not finish; retry Stop recording");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Self::join_recorder(t)
    }

    fn join_recorder(t: RecorderThread) -> Result<Option<(PathBuf, u64)>> {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !t.join.is_finished() {
            if Instant::now() > deadline {
                bail!("recorder did not finish (is the audio stream running?)");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let frames = t.join.join().map_err(|_| anyhow!("recorder thread panicked"))??;
        Ok(Some((t.path, frames)))
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let rec = self.recorder.lock().take();
        // Return and drop the core before joining the recorder and garbage collector.
        {
            let rt = self.runtime.get_mut();
            let _ = rt.reclaim();
            drop(rt.parked.take());
        }
        if let Some(t) = rec {
            let _ = Self::join_recorder(t);
        }
        self.collector_stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.collector.take() {
            let _ = j.join();
        }
    }
}

/// Page faults of the process so far (memory read back from swap, disk or the compressor).
fn major_faults() -> i64 {
    crate::sys::page_faults()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(channels: u16, min: u32, max: u32) -> cpal::SupportedStreamConfigRange {
        cpal::SupportedStreamConfigRange::new(channels, min, max,
            cpal::SupportedBufferSize::Range { min: 128, max: 1024 }, cpal::SampleFormat::F32)
    }

    #[test]
    fn legacy_audio_settings_keep_their_routing() {
        for routing in [HeadphoneRouting::Auto, HeadphoneRouting::Off, HeadphoneRouting::Split,
            HeadphoneRouting::Channels { master: [2, 3], cue: [0, 1] }] {
            let mut value = serde_json::to_value(AudioConfig { headphones: routing, ..AudioConfig::default() }).unwrap();
            let fields = value.as_object_mut().unwrap();
            fields.remove("headphone_device");
            fields.remove("alignment_ms");
            fields.remove("alignment_key");
            let config: AudioConfig = serde_json::from_value(value).unwrap();
            assert_eq!(config.headphones, routing);
            assert_eq!(config.headphone_device, None);
            assert_eq!(config.alignment_ms, 0.0);
        }
        let config = AudioConfig { device: Some("master-id".into()), headphone_device: Some("cue-id".into()),
            headphones: HeadphoneRouting::SeparateDevice, alignment_ms: -12.5, alignment_key: Some("pair".into()), ..AudioConfig::default() };
        assert_eq!(config, serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap());
    }

    #[test]
    fn device_ids_disambiguate_names_and_missing_selection_is_an_error() {
        let candidates = vec![("USB Audio".into(), "id-one".into()), ("USB Audio".into(), "id-two".into()), ("Speakers".into(), "id-three".into())];
        assert_eq!(resolve_device("id-two", &candidates).unwrap(), 1);
        assert_eq!(resolve_device("Speakers", &candidates).unwrap(), 2);
        assert!(resolve_device("USB Audio", &candidates).is_err());
        assert!(resolve_device("unplugged", &candidates).is_err());
    }

    #[test]
    fn correction_is_bound_to_the_resolved_pair_and_format() {
        let mut config = AudioConfig { alignment_ms: 12.5, ..AudioConfig::default() };
        bind_alignment(&mut config, "master-a/cue-b/48000/256".into());
        bind_alignment(&mut config, "master-a/cue-b/48000/256".into());
        assert_eq!(config.alignment_ms, 12.5);
        bind_alignment(&mut config, "master-a/cue-b/44100/256".into());
        assert_eq!(config.alignment_ms, 0.0);
        config.alignment_ms = -20.0;
        bind_alignment(&mut config, "master-a/cue-c/44100/256".into());
        assert_eq!(config.alignment_ms, 0.0);
    }

    #[test]
    fn negotiation_uses_exact_supported_channels_and_common_strict_format() {
        let master = [range(2, 44100, 48000), range(8, 44100, 48000)];
        let cue = [range(2, 48000, 48000)];
        let mut config = AudioConfig::default();
        assert_eq!(choose_format(&master, Some(&cue), &config, 2).unwrap(), (2, 48000));
        assert_eq!(choose_format(&master, None, &config, 4).unwrap(), (8, 44100));
        config.sample_rate = Some(44100);
        assert!(choose_format(&master, Some(&cue), &config, 2).is_err());
        config.sample_rate = None;
        config.buffer_frames = Some(64);
        assert!(choose_format(&master, Some(&cue), &config, 2).is_err());
        config.buffer_frames = None;
        assert_eq!(choose_format(&[range(4, 44100, 48000)], Some(&cue), &config, 2).unwrap(), (4, 48000));
        config.headphone_pair = [2, 3];
        assert!(choose_format(&master, Some(&cue), &config, 2).is_err());
        assert_eq!(choose_format(&master, Some(&[range(4, 48000, 48000)]), &config, 2).unwrap(), (2, 48000));
        config.headphone_pair = [0, 1];
        let integer_cue = [cpal::SupportedStreamConfigRange::new(2, 44100, 48000,
            cpal::SupportedBufferSize::Unknown, cpal::SampleFormat::I16)];
        assert!(choose_format(&master, Some(&integer_cue), &config, 2).is_err());
        assert!(choose_format(&master, Some(&[range(1, 44100, 48000)]), &config, 2).is_err());
    }

    #[test]
    fn single_device_rates_remain_preferences_and_buffers_are_validated_by_the_driver() {
        for routing in [HeadphoneRouting::Auto, HeadphoneRouting::Off, HeadphoneRouting::Split,
            HeadphoneRouting::Channels { master: [0, 1], cue: [2, 3] }] {
            let config = AudioConfig { headphones: routing, sample_rate: Some(96000), buffer_frames: Some(64), ..AudioConfig::default() };
            let channels = if matches!(routing, HeadphoneRouting::Channels { .. }) { 4 } else { 2 };
            assert_eq!(choose_format(&[range(channels, 48000, 48000)], None, &config, channels).unwrap(), (channels, 48000));
            assert_eq!(choose_format(&[range(channels, 44100, 48000)], None, &config, channels).unwrap(), (channels, 44100));
        }
        assert_eq!(default_format(&[range(2, 96000, 96000)], 2, 96000).unwrap(), (2, 96000));
        assert!(default_format(&[range(2, 96000, 96000)], 4, 96000).is_err());
        let strict = AudioConfig { headphones: HeadphoneRouting::SeparateDevice, sample_rate: Some(96000), ..AudioConfig::default() };
        assert!(choose_format(&[range(2, 48000, 48000)], Some(&[range(2, 48000, 48000)]), &strict, 2).is_err());
    }

    #[test]
    fn shared_wasapi_period_is_not_mistaken_for_a_fixed_request_limit() {
        let period = cpal::SupportedStreamConfigRange::new(2, 48000, 48000,
            cpal::SupportedBufferSize::Range { min: 480, max: 480 }, cpal::SampleFormat::F32);
        assert_eq!(supports_buffer(&period, Some(256)), cfg!(windows));
        assert!(supports_buffer(&period, None));
    }

    #[test]
    fn stopped_callback_returns_the_same_core_even_without_another_callback() {
        for callback_ran in [false, true] {
            let (_, commands) = rtrb::RingBuffer::new(4);
            let mut core = Box::new(EngineCore::new(48000, 2));
            core.apply(Command::SetMasterGain(-80.0));
            core.apply(Command::SetHeadphoneMix(0.25));
            let audio = Arc::new(clubcraft_audio::AudioBuffer::new(48000, vec![0.0; 48000], vec![0.0; 48000]));
            core.apply(Command::LoadTrack(0, Arc::new(crate::LoadedTrack::new("retained", audio))));
            core.apply(Command::Seek(0, 0.5));
            let pointer = std::ptr::from_ref(core.as_ref());
            let side = Box::new(AudioSide { core, cmds: commands, master: vec![], cue: vec![] });
            let (mut lend, incoming) = rtrb::RingBuffer::new(1);
            let (returns, mut returned) = rtrb::RingBuffer::new(2);
            let mut loan = CoreLoan { side: None, incoming, returns };
            lend.push(side).unwrap();
            if callback_ran { assert!(loan.get().is_some()); }
            drop(loan);
            let side = returned.pop().unwrap();
            assert_eq!(pointer, std::ptr::from_ref(side.core.as_ref()));
            assert_eq!(side.core.snapshot().master.headphone_mix, 0.25);
            assert!((side.core.snapshot().decks[0].position - 0.5).abs() < 1e-6);
        }
    }

    #[test]
    fn master_and_pfl_buses_keep_independent_gain_and_mix() {
        let mut core = EngineCore::new(48000, 2);
        core.apply(Command::SetLimiter { enabled: false, ceiling_db: -1.0 });
        for (deck, frequency) in [(0, 1000.0), (1, 500.0)] {
            let samples: Vec<_> = (0..96000).map(|f| (2.0 * std::f32::consts::PI * frequency * f as f32 / 48000.0).sin() * 0.1).collect();
            let audio = Arc::new(clubcraft_audio::AudioBuffer::new(48000, samples.clone(), samples));
            core.apply(Command::LoadTrack(deck, Arc::new(crate::LoadedTrack::new("bus", audio))));
            core.apply(Command::SetKeylock(deck, false));
            core.apply(Command::Play(deck));
        }
        core.apply(Command::SetFader(1, 0.0));
        core.apply(Command::SetPfl(1, true));
        core.apply(Command::SetHeadphoneMix(0.0));
        let mut master = [0.0; 960];
        let mut cue = [0.0; 960];
        for _ in 0..10 { core.process(&mut master, &mut cue); }
        assert!(master.iter().zip(cue).map(|(m, c)| (m - c).abs()).sum::<f32>() > 10.0);
        let reference = master;
        let cue_level: f32 = cue.iter().map(|s| s.abs()).sum();
        core.apply(Command::SetHeadphoneGain(-20.0));
        core.process(&mut master, &mut cue);
        assert!(master.iter().zip(reference).all(|(a, b)| (a - b).abs() < 0.001));
        assert!(cue.iter().map(|s| s.abs()).sum::<f32>() < cue_level * 0.2);
        core.apply(Command::SetHeadphoneGain(0.0));
        core.apply(Command::SetHeadphoneMix(1.0));
        core.process(&mut master, &mut cue);
        assert!(master.iter().zip(cue).all(|(m, c)| (m - c).abs() < 0.01));
    }
}
