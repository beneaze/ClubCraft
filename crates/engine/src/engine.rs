//! Real-time runtime: opens a cpal output stream and runs an [`EngineCore`] in its callback.
//!
//! * Commands: lock-free SPSC ring (rtrb) UI → audio. [`EngineHandle::send`] may be called from
//!   any thread (a mutex serialises *senders*; the audio thread never locks).
//! * State: the audio thread publishes an [`EngineState`] snapshot into a triple buffer after
//!   each callback; [`EngineHandle::state`] returns the latest copy.
//! * Garbage: tracks / schedules / recorder rings replaced on the audio thread are pushed to a
//!   ring that a collector thread drains and drops.
//! * Headphones: on devices with ≥ 4 outputs the cue bus goes to channels 3/4 (configurable);
//!   on stereo devices an optional split mode puts mono master left and mono cue right.

use crate::command::Command;
use crate::core::{EngineCore, Garbage};
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
    /// Channels 3/4 when the device has ≥ 4 outputs, otherwise no headphone output.
    #[default]
    Auto,
    /// Master only.
    Off,
    /// Explicit 0-based channel pairs.
    Channels { master: [u16; 2], cue: [u16; 2] },
    /// Stereo device with a splitter cable: mono master on the left, mono cue on the right.
    Split,
}

/// Audio output configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioConfig {
    /// Output device name (as listed by [`devices`]); `None` = system default.
    pub device: Option<String>,
    /// Preferred sample rate (44 100 or 48 000). `None` = 44.1 kHz if supported, else 48 kHz.
    pub sample_rate: Option<u32>,
    /// Requested buffer size in frames (`None` = device default). 256 ≈ 5.8 ms at 44.1 kHz.
    pub buffer_frames: Option<u32>,
    pub headphones: HeadphoneRouting,
    /// 2 or 4 decks.
    pub decks: usize,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig { device: None, sample_rate: None, buffer_frames: Some(256), headphones: HeadphoneRouting::Auto, decks: 4 }
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
    /// Output channel pairs used for master / cue (cue `None` = no headphone output).
    pub master_channels: [u16; 2],
    pub cue_channels: Option<[u16; 2]>,
    pub split: bool,
}

fn device_name(d: &cpal::Device) -> String {
    d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| "Unknown device".into())
}

/// List output devices.
pub fn devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default = host.default_output_device().map(|d| device_name(&d));
    let mut out = Vec::new();
    for d in host.output_devices()? {
        let name = device_name(&d);
        let id = d.id().map(|i| i.to_string()).unwrap_or_default();
        let mut max_ch = 0u16;
        let mut rates = Vec::new();
        if let Ok(cfgs) = d.supported_output_configs() {
            for c in cfgs {
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
        out.push(DeviceInfo { is_default: Some(&name) == default.as_ref(), name, id, max_channels: max_ch, sample_rates: rates });
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
    stream: Mutex<Option<cpal::Stream>>,
    eject: Mutex<Arc<AtomicBool>>,
    ret: Mutex<rtrb::Consumer<Box<AudioSide>>>,
    config: Mutex<AudioConfig>,
    info: Mutex<StreamInfo>,
    errors: Arc<AtomicU32>,
    collector_stop: Arc<AtomicBool>,
    collector: Option<std::thread::JoinHandle<()>>,
    recorder: Mutex<Option<RecorderThread>>,
}

/// Entry point.
pub struct Engine;

impl Engine {
    /// Open the output device and start the engine.
    pub fn start(config: AudioConfig) -> Result<EngineHandle> {
        let (gp, mut gc) = rtrb::RingBuffer::<Garbage>::new(1024);
        let (cp, cc) = rtrb::RingBuffer::<Command>::new(4096);
        let (rp, rc) = rtrb::RingBuffer::<Box<AudioSide>>::new(2);
        let (sin, sout) = triple_buffer::triple_buffer(&EngineState::default());
        let collector_stop = Arc::new(AtomicBool::new(false));
        let stop = collector_stop.clone();
        let collector = std::thread::Builder::new()
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
            .context("spawn collector")?;
        let mut core = Box::new(EngineCore::new(config.sample_rate.unwrap_or(44100), config.decks));
        core.set_garbage(gp);
        core.set_state_output(sin);
        let side = Box::new(AudioSide { core, cmds: cc, master: vec![0.0; SCRATCH_FRAMES * 2], cue: vec![0.0; SCRATCH_FRAMES * 2] });
        let errors = Arc::new(AtomicU32::new(0));
        let eject = Arc::new(AtomicBool::new(false));
        let (stream, info, _) = open_stream(&config, side, eject.clone(), rp, errors.clone())?;
        Ok(EngineHandle {
            sender: CommandSender { inner: Arc::new(Mutex::new(cp)) },
            state: Mutex::new(sout),
            stream: Mutex::new(Some(stream)),
            eject: Mutex::new(eject),
            ret: Mutex::new(rc),
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
        for d in host.output_devices()? {
            if device_name(&d) == n || d.id().map(|i| i.to_string() == n).unwrap_or(false) {
                return Ok(d);
            }
        }
        log::warn!("audio device {n:?} not found, using the default output");
    }
    host.default_output_device().ok_or_else(|| anyhow!("no audio output device"))
}

type OpenResult = (cpal::Stream, StreamInfo, Option<rtrb::Producer<Box<AudioSide>>>);

fn open_stream(
    config: &AudioConfig,
    mut side: Box<AudioSide>,
    eject: Arc<AtomicBool>,
    ret: rtrb::Producer<Box<AudioSide>>,
    errors: Arc<AtomicU32>,
) -> Result<OpenResult> {
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
        HeadphoneRouting::Auto | HeadphoneRouting::Off => (2, [0, 1], None, false),
        HeadphoneRouting::Channels { master, cue } => {
            let need = master.iter().chain(cue.iter()).copied().max().unwrap_or(1) + 1;
            if need > max_ch {
                bail!("device {name} has only {max_ch} outputs");
            }
            (need.max(2), master, Some(cue), false)
        }
        HeadphoneRouting::Split => (2, [0, 1], Some([0, 1]), true),
    };
    let preferred: Vec<u32> = match config.sample_rate {
        Some(r) => vec![r, 44100, 48000],
        None => vec![44100, 48000],
    };
    let chosen = preferred
        .iter()
        .find(|r| ranges.iter().any(|c| c.channels() >= want_ch && c.contains_rate(**r)))
        .copied();
    let (channels, sr) = match chosen {
        Some(r) => (want_ch, r),
        None => {
            let d = device.default_output_config()?;
            (d.channels().max(2), d.sample_rate())
        }
    };
    if side.core.sample_rate() != sr {
        side.core.set_sample_rate(sr);
    }
    let buffer_size = match config.buffer_frames {
        Some(f) => cpal::BufferSize::Fixed(f),
        None => cpal::BufferSize::Default,
    };
    let stream_config = cpal::StreamConfig { channels, sample_rate: sr, buffer_size };
    let ch = channels as usize;
    let cell = std::cell::UnsafeCell::new(Some(side));
    struct SendCell(std::cell::UnsafeCell<Option<Box<AudioSide>>>);
    // SAFETY: only the audio callback touches the cell after construction.
    unsafe impl Send for SendCell {}
    let cell = SendCell(cell);
    let mut ret = Some(ret);
    let mut load = 0f32;
    let mut health = crate::state::AudioHealth::default();
    let mut last_start: Option<Instant> = None;
    let mut window = (0f32, 0f32); // (peak in the current ~2 s window, seconds in it)
    let mc = master_ch;
    let cc = cue_ch;
    let err2 = errors.clone();
    let data_cb = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let t0 = Instant::now();
        let faults0 = major_faults();
        // SAFETY: exclusive access from the callback.
        let slot = unsafe { &mut *cell.0.get() };
        if eject.load(Ordering::Acquire) {
            if let (Some(s), Some(r)) = (slot.take(), ret.as_mut()) {
                let _ = r.push(s);
            }
            data.fill(0.0);
            return;
        }
        let Some(side) = slot.as_mut() else {
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
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(SCRATCH_FRAMES);
            let (m, c) = (&mut side.master[..n * 2], &mut side.cue[..n * 2]);
            side.core.process(m, c);
            let out = &mut data[done * ch..(done + n) * ch];
            out.fill(0.0);
            for f in 0..n {
                let o = &mut out[f * ch..(f + 1) * ch];
                if split {
                    o[0] = 0.5 * (m[f * 2] + m[f * 2 + 1]);
                    o[1] = 0.5 * (c[f * 2] + c[f * 2 + 1]);
                } else {
                    o[mc[0] as usize] = m[f * 2];
                    o[mc[1] as usize] = m[f * 2 + 1];
                    if let Some(cc) = cc {
                        o[cc[0] as usize] = c[f * 2];
                        o[cc[1] as usize] = c[f * 2 + 1];
                    }
                }
            }
            done += n;
        }
        let budget = frames as f32 / sr as f32;
        let l = t0.elapsed().as_secs_f32() / budget.max(1e-6);
        load = load * 0.9 + l * 0.1;
        side.core.cpu_load = load;
        // health: overruns, late callbacks, peaks
        health.buffer_frames = frames as u32;
        if l > 1.0 {
            health.overruns += 1;
        }
        if major_faults() > faults0 + 8 {
            health.faulted += 1;
            if l > 1.0 {
                health.faulted_overruns += 1;
            }
        }
        if let Some(prev) = last_start {
            if t0.duration_since(prev).as_secs_f32() > 2.0 * budget + 0.002 {
                health.late += 1;
            }
        }
        last_start = Some(t0);
        health.peak_load = health.peak_load.max(l);
        window.0 = window.0.max(l);
        window.1 += budget;
        health.recent_peak = health.recent_peak.max(window.0);
        if window.1 >= 2.0 {
            health.recent_peak = window.0;
            window = (0.0, 0.0);
        }
        let sections = side.core.audio.section_peak_us;
        for (k, v) in crate::prof::take().into_iter().enumerate() {
            health.deck_peak_us[k] = health.deck_peak_us[k].max(v);
        }
        side.core.audio = health;
        side.core.audio.section_peak_us = sections;
        side.core.stream_errors = err2.load(Ordering::Relaxed);
    };
    let err_cb = move |e: cpal::Error| {
        log::warn!("audio stream error: {e}");
        errors.fetch_add(1, Ordering::Relaxed);
    };
    let stream = device
        .build_output_stream::<f32, _, _>(stream_config, data_cb, err_cb, Some(Duration::from_secs(3)))
        .with_context(|| format!("open {name} ({channels} ch @ {sr} Hz)"))?;
    stream.play().context("start stream")?;
    let info = StreamInfo {
        device: name,
        sample_rate: sr,
        channels,
        buffer_frames: config.buffer_frames,
        master_channels: master_ch,
        cue_channels: cue_ch,
        split,
    };
    Ok((stream, info, None))
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
        *self.state.lock().read()
    }

    pub fn stream_info(&self) -> StreamInfo {
        self.info.lock().clone()
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
    pub fn set_audio_config(&self, config: AudioConfig) -> Result<()> {
        let mut stream_guard = self.stream.lock();
        // ask the callback to hand back the engine
        let eject = self.eject.lock().clone();
        eject.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_millis(1000);
        let mut side = None;
        while Instant::now() < deadline {
            if let Ok(s) = self.ret.lock().pop() {
                side = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(stream_guard.take());
        let side = side.ok_or_else(|| anyhow!("audio callback did not respond; engine state lost"))?;
        let (rp, rc) = rtrb::RingBuffer::<Box<AudioSide>>::new(2);
        *self.ret.lock() = rc;
        let new_eject = Arc::new(AtomicBool::new(false));
        let mut side = side;
        side.core.apply(Command::SetDeckCount(config.decks));
        let (stream, info, _) = open_stream(&config, side, new_eject.clone(), rp, self.errors.clone())?;
        *stream_guard = Some(stream);
        *self.eject.lock() = new_eject;
        *self.info.lock() = info;
        *self.config.lock() = config;
        Ok(())
    }

    /// Start recording the master output to a WAV file.
    pub fn start_recording(&self, path: &Path, format: WavFormat) -> Result<()> {
        self.stop_recording()?;
        let sr = self.info.lock().sample_rate;
        let (p, c) = rtrb::RingBuffer::<f32>::new(sr as usize * 2 * 10);
        let t = RecorderThread::spawn(path.to_path_buf(), sr, format, c)?;
        self.send(Command::SetRecorder(Some(p)))?;
        *self.recorder.lock() = Some(t);
        Ok(())
    }

    /// Stop recording; returns the file path and number of frames written.
    pub fn stop_recording(&self) -> Result<Option<(PathBuf, u64)>> {
        let Some(t) = self.recorder.lock().take() else { return Ok(None) };
        self.send(Command::SetRecorder(None))?;
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
        // dropping the stream drops the engine (and the recorder ring), finishing the file
        drop(self.stream.lock().take());
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
    #[cfg(unix)]
    {
        // SAFETY: getrusage only writes the struct.
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } == 0 {
            // minor faults include pages macOS decompresses from its memory compressor
            return u.ru_majflt as i64 + u.ru_minflt as i64;
        }
    }
    0
}
