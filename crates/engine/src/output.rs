//! Independent output clocks. Only the master callback advances the engine.
//! All storage and sinc phases are allocated before either stream starts.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

const TAPS: usize = 32;
const PHASES: usize = 512;
// Leave a transition band below Nyquist for the 32-tap filter at +/-0.5% drift.
const SINC_CUTOFF: f64 = 0.94;
// Queue error is measured in frames: 500 frames asks for 1000 ppm proportional
// correction. The integral removes steady clock error over tens of seconds.
const SERVO_PROPORTIONAL: f64 = 2e-6;
const SERVO_INTEGRAL: f64 = 1e-7;
// Two seconds rejects callback jitter. Reserve 4000 ppm for integral trim and
// cap total correction at 5000 ppm, comfortably above the tested +/-1000 ppm.
const SERVO_TAU_SECONDS: f64 = 2.0;
const SERVO_INTEGRAL_LIMIT: f64 = 0.004;
const SERVO_RATIO_LIMIT: f64 = 0.005;
// Count rendered cue time rather than wall time: waiting for a device or
// rebuffering must not make the first irregular callback the final calibration.
const ALIGNMENT_WARMUP_SECONDS: u64 = 2;
const ERROR_KINDS: [cpal::ErrorKind; 14] = [
    cpal::ErrorKind::DeviceBusy,
    cpal::ErrorKind::DeviceChanged,
    cpal::ErrorKind::DeviceNotAvailable,
    cpal::ErrorKind::HostUnavailable,
    cpal::ErrorKind::InvalidInput,
    cpal::ErrorKind::PermissionDenied,
    cpal::ErrorKind::RealtimeDenied,
    cpal::ErrorKind::ResourceExhausted,
    cpal::ErrorKind::StreamInvalidated,
    cpal::ErrorKind::UnsupportedConfig,
    cpal::ErrorKind::UnsupportedOperation,
    cpal::ErrorKind::Xrun,
    cpal::ErrorKind::BackendError,
    cpal::ErrorKind::Other,
];

pub(crate) struct EndpointHealth {
    pub frames: AtomicU32,
    pub latency_us: AtomicU32,
    pub errors: AtomicU32,
    pub failed: AtomicBool,
    pending_errors: AtomicU32,
    created: Instant,
    last_callback_us: AtomicU64,
    armed: AtomicBool,
}

impl Default for EndpointHealth {
    fn default() -> Self {
        Self {
            frames: AtomicU32::new(0),
            latency_us: AtomicU32::new(0),
            errors: AtomicU32::new(0),
            failed: AtomicBool::new(false),
            pending_errors: AtomicU32::new(0),
            created: Instant::now(),
            last_callback_us: AtomicU64::new(0),
            armed: AtomicBool::new(false),
        }
    }
}

impl EndpointHealth {
    /// Error callbacks may run on the audio thread: keep diagnostics allocation-free.
    pub fn report_error(&self, kind: cpal::ErrorKind) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        let index = ERROR_KINDS.iter().position(|k| *k == kind).unwrap_or(ERROR_KINDS.len() - 1);
        self.pending_errors.fetch_or(1 << index, Ordering::Relaxed);
        // BackendError has no reliable fatality classification. Let callback
        // liveness diagnose it rather than permanently muting a healthy stream.
        if !matches!(
            kind,
            cpal::ErrorKind::Xrun
                | cpal::ErrorKind::DeviceChanged
                | cpal::ErrorKind::RealtimeDenied
                | cpal::ErrorKind::DeviceBusy
                | cpal::ErrorKind::ResourceExhausted
                | cpal::ErrorKind::BackendError
        ) {
            self.failed.store(true, Ordering::Release);
        }
    }

    /// Called only outside audio callbacks; coalesce repeated xruns between polls.
    pub fn log_errors(&self, name: &str) {
        let pending = self.pending_errors.swap(0, Ordering::Relaxed);
        for (index, kind) in ERROR_KINDS.iter().enumerate() {
            if pending & (1 << index) != 0 {
                log::warn!("Audio output {name}: {kind}");
            }
        }
    }

    pub fn heartbeat(&self, frames: usize) {
        self.last_callback_us.store(self.created.elapsed().as_micros() as u64, Ordering::Relaxed);
        self.frames.store(frames as u32, Ordering::Relaxed);
    }

    pub fn arm(&self) {
        self.last_callback_us.store(self.created.elapsed().as_micros() as u64, Ordering::Relaxed);
        self.armed.store(true, Ordering::Release);
    }

    pub fn check_stalled(&self, sr: u32) -> bool {
        let budget_us = self.frames.load(Ordering::Relaxed) as u64 * 1_000_000 / sr.max(1) as u64;
        let age = (self.created.elapsed().as_micros() as u64).saturating_sub(self.last_callback_us.load(Ordering::Relaxed));
        // A stall is a live observation, not a terminal error. A resumed callback
        // updates the heartbeat and immediately restores this status.
        self.failed.load(Ordering::Acquire) || (self.armed.load(Ordering::Acquire) && age > (budget_us * 8).max(2_000_000))
    }
}

#[derive(Default)]
pub(crate) struct OutputHealth {
    pub master: EndpointHealth,
    pub cue: EndpointHealth,
    pub underruns: AtomicU32,
    pub overflows: AtomicU32,
    pub reset: AtomicBool,
    pub master_delay: AtomicU32,
    pub cue_delay: AtomicU32,
    pub offset_us: AtomicI32,
}

/// Zero means unavailable, including backends which supply zero timestamps.
pub(crate) fn observe(info: &cpal::OutputCallbackInfo, frames: usize, health: &EndpointHealth) -> Option<u64> {
    health.heartbeat(frames);
    let t = info.timestamp();
    let us = t
        .playback
        .checked_duration_since(t.callback)
        .filter(|d| !d.is_zero() && d.as_secs_f64() < 2.0)
        .map(|d| d.as_micros() as u32)
        .unwrap_or(0);
    health.latency_us.store(us, Ordering::Relaxed);
    (us != 0).then_some(us as u64 * 1000)
}

#[derive(Clone, Copy, Default)]
pub(crate) struct CueFrame {
    pub samples: [f32; 2],
    /// Predicted, undelayed master presentation time in the shared Instant epoch.
    pub master_ns: u64,
}

/// A bounded fractional stereo delay with fixed read positions. A tap change
/// crossfades over 128 frames; moving a read position would bend the pitch.
pub(crate) struct Delay {
    frames: Vec<[f32; 2]>,
    write: usize,
    delay: f64,
    previous: f64,
    fade: f32,
}

impl Delay {
    pub fn new(sr: u32) -> Self {
        Self {
            frames: vec![[0.0; 2]; sr as usize * 2 + 2],
            write: 0,
            delay: 0.0,
            previous: 0.0,
            fade: 1.0,
        }
    }

    pub fn process(&mut self, frame: [f32; 2], target: f64) -> [f32; 2] {
        let target = target.clamp(0.0, (self.frames.len() - 2) as f64);
        if target != self.delay {
            self.previous = self.delay;
            self.delay = target;
            self.fade = 0.0;
        }
        self.frames[self.write] = frame;
        let current = self.read(self.delay);
        let previous = self.read(self.previous);
        self.fade = (self.fade + 1.0 / 128.0).min(1.0);
        let out = std::array::from_fn(|i| previous[i] + (current[i] - previous[i]) * self.fade);
        self.write = (self.write + 1) % self.frames.len();
        out
    }

    fn read(&self, delay: f64) -> [f32; 2] {
        let read = (self.write as f64 - delay).rem_euclid(self.frames.len() as f64);
        let base = read.floor();
        // rem_euclid may round a tiny negative input up to exactly len.
        let a = base as usize % self.frames.len();
        let b = (a + 1) % self.frames.len();
        let t = (read - base) as f32;
        std::array::from_fn(|i| self.frames[a][i] + (self.frames[b][i] - self.frames[a][i]) * t)
    }
}

/// PI correction is intentionally slow: callback scheduling jitter is not clock drift.
#[derive(Default)]
struct ClockServo {
    error: f64,
    integral: f64,
    ratio: f64,
}

impl ClockServo {
    fn update(&mut self, error: f64, seconds: f64) -> f64 {
        let smoothing = 1.0 - (-seconds / SERVO_TAU_SECONDS).exp();
        self.error += (error - self.error) * smoothing;
        self.integral = (self.integral + self.error * seconds * SERVO_INTEGRAL).clamp(-SERVO_INTEGRAL_LIMIT, SERVO_INTEGRAL_LIMIT);
        let desired_ratio = (self.error * SERVO_PROPORTIONAL + self.integral).clamp(-SERVO_RATIO_LIMIT, SERVO_RATIO_LIMIT);
        self.ratio += (desired_ratio - self.ratio) * smoothing;
        1.0 + self.ratio
    }
}

pub(crate) fn alignment(offset_ns: f64, correction_ms: f64, sr: u32) -> (f64, f64) {
    let frames = (offset_ns * 1e-9 + correction_ms * 0.001) * sr as f64;
    (frames.clamp(0.0, sr as f64 * 2.0), (-frames).clamp(0.0, sr as f64 * 2.0))
}

/// Physical-output protection is deliberately downstream of the recorded master bus.
pub(crate) fn mix_shared_output(master: &[f32], cue: &[f32], output: &mut [f32], limiter: &mut crate::dsp::Limiter) {
    for ((out, master), cue) in output.iter_mut().zip(master).zip(cue) { *out = master + cue; }
    limiter.process(output);
}

pub(crate) struct CueOutput {
    input: rtrb::Consumer<CueFrame>,
    health: Arc<OutputHealth>,
    epoch: Instant,
    sr: u32,
    correction_ms: f64,
    history: [CueFrame; TAPS],
    head: usize,
    phase: f64,
    kernel: Vec<[f32; TAPS]>,
    servo: ClockServo,
    started: bool,
    gain: f32,
    last: [f32; 2],
    delay: Delay,
    offset_ns: Option<f64>,
    estimate_frames: u64,
    estimate_has_timestamps: Option<bool>,
    alignment_set: bool,
}

impl CueOutput {
    pub fn new(input: rtrb::Consumer<CueFrame>, health: Arc<OutputHealth>, epoch: Instant, sr: u32, correction_ms: f64) -> Self {
        let kernel = (0..=PHASES)
            .map(|p| {
                let frac = p as f64 / PHASES as f64;
                let mut row = std::array::from_fn(|k| {
                    let x = k as f64 - (TAPS / 2 - 1) as f64 - frac;
                    let sinc = if x.abs() < 1e-10 {
                        SINC_CUTOFF
                    } else {
                        (std::f64::consts::PI * x * SINC_CUTOFF).sin() / (std::f64::consts::PI * x)
                    };
                    // Blackman window limits sidelobes of the short sinc kernel.
                    let angle = std::f64::consts::PI * x / (TAPS / 2) as f64;
                    let window = 0.42 + 0.5 * angle.cos() + 0.08 * (2.0 * angle).cos();
                    (sinc * window) as f32
                });
                let sum: f32 = row.iter().sum();
                for c in &mut row {
                    *c /= sum;
                }
                row
            })
            .collect();
        Self {
            input,
            health,
            epoch,
            sr,
            correction_ms,
            history: [CueFrame::default(); TAPS],
            head: 0,
            phase: 0.0,
            kernel,
            servo: ClockServo::default(),
            started: false,
            gain: 0.0,
            last: [0.0; 2],
            delay: Delay::new(sr),
            offset_ns: None,
            estimate_frames: 0,
            estimate_has_timestamps: None,
            alignment_set: false,
        }
    }

    pub fn render_channels(&mut self, data: &mut [f32], info: &cpal::OutputCallbackInfo, channels: usize, pair: [u16; 2]) {
        let frames = data.len() / channels;
        let latency = observe(info, frames, &self.health.cue);
        let now_ns = self.epoch.elapsed().as_nanos() as u64;
        self.render_at_channels(data, latency.map(|ns| now_ns + ns), channels, pair);
    }

    #[cfg(test)]
    fn render_at(&mut self, data: &mut [f32], playback_ns: Option<u64>) {
        self.render_at_channels(data, playback_ns, 2, [0, 1]);
    }

    fn render_at_channels(&mut self, data: &mut [f32], playback_ns: Option<u64>, channels: usize, pair: [u16; 2]) {
        let frames = data.len() / channels;
        data.fill(0.0);
        let block = frames.max(self.health.master.frames.load(Ordering::Relaxed) as usize);
        let target = (self.sr as usize / 50).max(block * 2);
        if target + frames + TAPS > self.sr as usize * 2 {
            // A pathological driver buffer must not leave cue waiting forever
            // for more data than the bounded transport can hold.
            self.health.cue.failed.store(true, Ordering::Release);
            data.fill(0.0);
            return;
        }
        let reset = self.health.reset.swap(false, Ordering::Relaxed);
        if reset || self.input.slots() > target + block * 6 + TAPS {
            if !reset {
                self.health.overflows.fetch_add(1, Ordering::Relaxed);
            }
            self.started = false;
            self.reset_estimate();
            while self.input.slots() > target + frames + TAPS {
                let _ = self.input.pop();
            }
        }
        if !self.started && self.gain == 0.0 && self.input.slots() >= target + frames + TAPS {
            for h in &mut self.history {
                *h = self.input.pop().unwrap_or_default();
            }
            self.head = 0;
            self.phase = 0.0;
            self.servo = ClockServo::default();
            self.reset_estimate();
            self.started = true;
        }
        let ratio = self
            .servo
            .update(self.input.slots() as f64 - frames as f64 - target as f64, frames as f64 / self.sr as f64);
        let mut measured = false;
        for (f, out) in data.chunks_exact_mut(channels).enumerate() {
            if self.started {
                while self.phase >= 1.0 {
                    if let Ok(h) = self.input.pop() {
                        self.history[self.head] = h;
                        self.head = (self.head + 1) % TAPS;
                        self.phase -= 1.0;
                    } else {
                        self.started = false;
                        self.health.underruns.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                }
            }
            if self.started {
                let phase_position = self.phase * PHASES as f64;
                let phase_index = phase_position as usize;
                let phase_blend = (phase_position - phase_index as f64) as f32;
                let mut sample = [0.0; 2];
                for k in 0..TAPS {
                    let frame = self.history[(self.head + k) % TAPS];
                    let coefficient = self.kernel[phase_index][k] + (self.kernel[phase_index + 1][k] - self.kernel[phase_index][k]) * phase_blend;
                    sample[0] += frame.samples[0] * coefficient;
                    sample[1] += frame.samples[1] * coefficient;
                }
                if !measured {
                    let h = self.history[(self.head + TAPS / 2 - 1) % TAPS];
                    if let Some(playback) = playback_ns.filter(|_| h.master_ns != 0) {
                        let offset = playback as f64 + (f as f64 - self.phase) * 1e9 / self.sr as f64 - h.master_ns as f64;
                        self.set_alignment(offset, frames, true);
                    } else {
                        // Without driver timing we still know the queued frames
                        // ahead of the sample at the sinc center. Include all of
                        // them, not just the configured target fill.
                        let queued = self.input.slots() as f64 + (TAPS / 2 + 1) as f64 - self.phase;
                        self.set_alignment(queued * 1e9 / self.sr as f64, frames, false);
                    }
                    measured = true;
                }
                self.last = sample;
                self.phase += ratio;
            }
            let want = if self.started { 1.0 } else { 0.0 };
            self.gain += (want - self.gain).clamp(-1.0 / 128.0, 1.0 / 128.0);
            let delayed = self.delay.process(self.last, self.health.cue_delay.load(Ordering::Relaxed) as f64);
            out[pair[0] as usize] = delayed[0] * self.gain;
            out[pair[1] as usize] = delayed[1] * self.gain;
        }
    }

    fn reset_estimate(&mut self) {
        self.offset_ns = None;
        self.estimate_frames = 0;
        self.estimate_has_timestamps = None;
    }

    fn set_alignment(&mut self, offset_ns: f64, frames: usize, has_timestamps: bool) {
        if frames == 0 {
            return;
        }
        if self.estimate_has_timestamps != Some(has_timestamps) {
            self.reset_estimate();
            self.estimate_has_timestamps = Some(has_timestamps);
        }
        self.estimate_frames = self.estimate_frames.saturating_add(frames as u64);
        // Average the first second by sample count, then use a one-second low
        // pass. This prevents the first callback from dominating the estimate
        // while giving startup irregularities two seconds to settle.
        let smoothing = (frames as f64 / self.estimate_frames.min(self.sr as u64) as f64).min(1.0);
        let filtered = self.offset_ns.map(|old| old + (offset_ns - old) * smoothing).unwrap_or(offset_ns);
        self.offset_ns = Some(filtered);
        self.health
            .offset_us
            .store(if has_timestamps { (filtered / 1000.0) as i32 } else { i32::MIN }, Ordering::Relaxed);
        // Latch once after warmup. Queue jitter and rebuffering must never
        // move the master tap. Apply (including correction changes) recalibrates;
        // all continuous clock correction stays in the cue resampler.
        if !self.alignment_set && self.estimate_frames >= self.sr as u64 * ALIGNMENT_WARMUP_SECONDS {
            let (master, cue) = alignment(filtered, self.correction_ms, self.sr);
            self.health.master_delay.store(master.round() as u32, Ordering::Relaxed);
            self.health.cue_delay.store(cue.round() as u32, Ordering::Relaxed);
            self.alignment_set = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_that_stops_callbacks_is_reported_even_without_an_error_callback() {
        let endpoint = EndpointHealth::default();
        assert!(!endpoint.check_stalled(48000));
        endpoint.arm();
        assert!(!endpoint.check_stalled(48000));
        let stale = EndpointHealth {
            created: Instant::now().checked_sub(std::time::Duration::from_secs(10)).unwrap(),
            armed: AtomicBool::new(true),
            ..EndpointHealth::default()
        };
        assert!(stale.check_stalled(48000));
        assert!(!stale.failed.load(Ordering::Acquire));
        stale.heartbeat(256);
        assert!(!stale.check_stalled(48000));
        stale.report_error(cpal::ErrorKind::StreamInvalidated);
        stale.heartbeat(256);
        assert!(stale.check_stalled(48000));
    }

    #[test]
    fn transient_errors_keep_both_outputs_running_and_terminal_errors_stay_failed() {
        let health = OutputHealth::default();
        for endpoint in [&health.master, &health.cue] {
            endpoint.arm();
            for kind in [
                cpal::ErrorKind::Xrun,
                cpal::ErrorKind::DeviceChanged,
                cpal::ErrorKind::RealtimeDenied,
                cpal::ErrorKind::DeviceBusy,
                cpal::ErrorKind::ResourceExhausted,
                cpal::ErrorKind::BackendError,
            ] {
                endpoint.report_error(kind);
                endpoint.heartbeat(256);
                assert!(!endpoint.check_stalled(48000), "{kind:?}");
            }
            assert_eq!(endpoint.errors.load(Ordering::Relaxed), 6);
            assert_ne!(endpoint.pending_errors.load(Ordering::Relaxed), 0);
            endpoint.log_errors("test output");
            assert_eq!(endpoint.pending_errors.load(Ordering::Relaxed), 0);
        }
        health.cue.report_error(cpal::ErrorKind::DeviceNotAvailable);
        assert!(health.cue.check_stalled(48000));
        assert!(!health.master.check_stalled(48000));
        for kind in [
            cpal::ErrorKind::DeviceNotAvailable,
            cpal::ErrorKind::StreamInvalidated,
            cpal::ErrorKind::HostUnavailable,
            cpal::ErrorKind::PermissionDenied,
            cpal::ErrorKind::Other,
        ] {
            let endpoint = EndpointHealth::default();
            endpoint.report_error(kind);
            endpoint.heartbeat(256);
            assert!(endpoint.check_stalled(48000), "{kind:?}");
        }
    }

    #[test]
    fn master_alignment_does_not_follow_jitter_or_rebuffering() {
        let health = Arc::new(OutputHealth::default());
        let (_, input) = rtrb::RingBuffer::new(96000);
        let mut cue = CueOutput::new(input, health.clone(), Instant::now(), 48000, 5.0);
        cue.set_alignment(25_000_000.0, 48000, true);
        assert_eq!(health.master_delay.load(Ordering::Relaxed), 0);
        cue.set_alignment(25_000_000.0, 48000, true);
        let delay = health.master_delay.load(Ordering::Relaxed);
        assert_eq!(delay, 1440);
        for offset in [35_000_000.0, -10_000_000.0, 120_000_000.0] {
            cue.set_alignment(offset, 256, true);
            health.reset.store(true, Ordering::Relaxed);
            cue.render_at(&mut [0.0; 512], None);
            assert_eq!(health.master_delay.load(Ordering::Relaxed), delay);
        }
    }

    #[test]
    fn alignment_averages_startup_outliers_and_waits_for_two_seconds() {
        let health = Arc::new(OutputHealth::default());
        let (_, input) = rtrb::RingBuffer::new(96000);
        let mut cue = CueOutput::new(input, health.clone(), Instant::now(), 48000, 0.0);
        cue.set_alignment(120_000_000.0, 256, true);
        for _ in 1..374 {
            cue.set_alignment(20_000_000.0, 256, true);
            assert!(!cue.alignment_set);
            assert_eq!(health.master_delay.load(Ordering::Relaxed), 0);
        }
        cue.set_alignment(20_000_000.0, 256, true);
        assert!(cue.alignment_set);
        let frames = health.master_delay.load(Ordering::Relaxed);
        assert!((frames as f64 / 48.0 - 20.0).abs() < 1.0);
        cue.set_alignment(120_000_000.0, 96000, true);
        assert_eq!(health.master_delay.load(Ordering::Relaxed), frames);
    }

    #[test]
    fn calibration_warmup_restarts_on_rebuffering_or_timing_source_change() {
        let health = Arc::new(OutputHealth::default());
        let (_, input) = rtrb::RingBuffer::new(96000);
        let mut cue = CueOutput::new(input, health.clone(), Instant::now(), 48000, -10.0);
        cue.set_alignment(100_000_000.0, 72000, true);
        health.reset.store(true, Ordering::Relaxed);
        cue.render_at(&mut [0.0; 512], None);
        assert_eq!(cue.estimate_frames, 0);
        cue.set_alignment(100_000_000.0, 72000, true);
        cue.set_alignment(30_000_000.0, 48000, false);
        assert!(!cue.alignment_set);
        assert_eq!(cue.estimate_frames, 48000);
        assert_eq!(health.offset_us.load(Ordering::Relaxed), i32::MIN);
        cue.set_alignment(30_000_000.0, 48000, false);
        assert_eq!(health.master_delay.load(Ordering::Relaxed), 960);
        // Losing timestamps after calibration affects diagnostics, not the master tap.
        cue.set_alignment(200_000_000.0, 96000, true);
        assert_eq!(health.master_delay.load(Ordering::Relaxed), 960);
    }

    #[test]
    fn a_master_delay_change_crossfades_fixed_taps_without_resampling() {
        let mut delay = Delay::new(48000);
        for frame in 0..2000 {
            let sample = (frame as f32 * 0.13).sin();
            delay.process([sample; 2], 0.0);
        }
        for frame in 2000..2500 {
            let sample = (frame as f32 * 0.13).sin();
            let out = delay.process([sample; 2], 1200.0);
            let blend = ((frame - 2000 + 1) as f32 / 128.0).min(1.0);
            let delayed = ((frame - 1200) as f32 * 0.13).sin();
            assert!((out[0] - (sample + (delayed - sample) * blend)).abs() < 1e-6);
            assert_eq!(delay.delay, 1200.0);
        }
    }

    #[test]
    fn one_hour_of_independent_clocks_and_callback_jitter() {
        for ppm in [-1000.0, 1000.0] {
            let mut servo = ClockServo::default();
            let mut fill = 960.0;
            let mut consumed = 0.0;
            let sr = 48000.0;
            let mut peak: f64 = 0.0;
            for block in 0..675000 {
                let n = [128.0, 512.0, 256.0, 128.0][block % 4];
                let dt = n / (sr * (1.0 + ppm * 1e-6));
                // Bounded scheduling jitter, with no net long-term clock change.
                let jitter = if block % 2 == 0 { 16.0 } else { -16.0 };
                let ratio = servo.update(fill - 960.0 + jitter, dt);
                fill += dt * sr - n * ratio;
                consumed += dt;
                peak = peak.max((fill - 960.0).abs());
                assert!((400.0..1500.0).contains(&fill));
            }
            assert!(consumed >= 3590.0);
            assert!(peak < 500.0);
            assert!((servo.ratio + ppm * 1e-6).abs() < 0.00002);
        }
    }

    #[test]
    fn alignment_sign_and_fractional_delay() {
        assert_eq!(alignment(20_000_000.0, 5.0, 48000), (1200.0, 0.0));
        let (m, c) = alignment(20_000_000.0, -30.0, 48000);
        assert_eq!(m, 0.0);
        assert!((c - 480.0).abs() < 1e-8);
        for target in [0.0, 1e-12, 17.5, 240.25, 1200.0, 24000.0] {
            let mut delay = Delay::new(48000);
            for _ in 0..100000 {
                delay.process([0.0; 2], target);
            }
            let mut peak = (0, 0.0f32);
            for f in 0..25000 {
                let out = delay.process(if f == 0 { [1.0, -1.0] } else { [0.0; 2] }, target);
                assert_eq!(out[0], -out[1]);
                if out[0] > peak.1 {
                    peak = (f, out[0]);
                }
            }
            assert!((peak.0 as f64 - target).abs() < 1.0);
        }
    }

    #[test]
    fn cue_timestamps_include_queue_and_filter_lookahead_in_alignment() {
        for correction in [-250.0, 0.0, 20.0] {
            let health = Arc::new(OutputHealth::default());
            health.master.frames.store(256, Ordering::Relaxed);
            let (mut tx, rx) = rtrb::RingBuffer::new(96000);
            let mut cue = CueOutput::new(rx, health.clone(), Instant::now(), 48000, correction);
            // A synthetic impulse at the center tap is presented by master at
            // 1 s + 15 frames. The headphone driver schedules it at 1.2 s.
            for f in 0..2000 {
                tx.push(CueFrame {
                    samples: if f == 15 { [1.0; 2] } else { [0.0; 2] },
                    master_ns: 1_000_000_000 + f * 1_000_000_000 / 48000,
                })
                .unwrap();
            }
            cue.render_at(&mut [0.0; 512], Some(1_200_000_000));
            assert!(!cue.alignment_set);
            let measured = cue.offset_ns.unwrap();
            for _ in 1..375 {
                cue.set_alignment(measured, 256, true);
            }
            assert!(cue.alignment_set);
            let m = health.master_delay.load(Ordering::Relaxed) as f64;
            let c = health.cue_delay.load(Ordering::Relaxed) as f64;
            let residual = (200.0 - 15.0 / 48.0 + correction) - (m - c) / 48.0;
            assert!(residual.abs() < 1.0, "alignment residual {residual} ms");
        }
    }

    #[test]
    fn missing_timestamps_and_manual_correction_are_explicit() {
        let health = Arc::new(OutputHealth::default());
        let info = cpal::OutputCallbackInfo::new(cpal::OutputStreamTimestamp {
            callback: cpal::StreamInstant::new(1, 0),
            playback: cpal::StreamInstant::new(1, 10_000_000),
        });
        assert_eq!(observe(&info, 128, &health.master), Some(10_000_000));
        assert_eq!(health.master.frames.load(Ordering::Relaxed), 128);
        let unknown = cpal::OutputCallbackInfo::new(cpal::OutputStreamTimestamp {
            callback: cpal::StreamInstant::ZERO,
            playback: cpal::StreamInstant::ZERO,
        });
        assert_eq!(observe(&unknown, 512, &health.cue), None);
        let (mut tx, rx) = rtrb::RingBuffer::new(96000);
        for _ in 0..2400 {
            tx.push(CueFrame::default()).unwrap();
        }
        let mut cue = CueOutput::new(rx, health.clone(), Instant::now(), 48000, -10.0);
        cue.render_at(&mut [0.0; 512], None);
        assert_eq!(health.offset_us.load(Ordering::Relaxed), i32::MIN);
        assert!(!cue.alignment_set);
        let measured = cue.offset_ns.unwrap();
        for _ in 1..375 {
            cue.set_alignment(measured, 256, false);
        }
        // At the first rendered sample there are 2368 frames still queued and
        // 17 frames of sinc history ahead. The -10 ms correction removes 480.
        assert_eq!(health.master_delay.load(Ordering::Relaxed), 2368 + 17 - 480);
        assert_eq!(health.cue_delay.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn shared_physical_output_sums_buses_and_limits_without_modifying_recorded_master() {
        let master = vec![0.7; 512];
        let cue = vec![0.6; 512];
        let mut output = vec![0.0; 512];
        let mut limiter = crate::dsp::Limiter::new(44100.0);
        for _ in 0..10 { mix_shared_output(&master, &cue, &mut output, &mut limiter); }
        assert!(output.iter().all(|v| v.is_finite() && v.abs() <= 1.0));
        assert!(output.iter().any(|v| *v > 0.7));
        assert!(master.iter().all(|v| *v == 0.7));
        assert!(cue.iter().all(|v| *v == 0.6));
    }

    #[test]
    fn cue_can_use_an_arbitrary_stereo_pair_without_touching_other_channels() {
        let health = Arc::new(OutputHealth::default());
        health.master.frames.store(256, Ordering::Relaxed);
        let (mut tx, rx) = rtrb::RingBuffer::new(96000);
        for _ in 0..2400 { tx.push(CueFrame { samples: [0.25, -0.5], master_ns: 0 }).unwrap(); }
        let mut cue = CueOutput::new(rx, health, Instant::now(), 48000, 0.0);
        let mut data = vec![9.0; 256 * 6];
        cue.render_at_channels(&mut data, None, 6, [4, 5]);
        assert!(data.as_chunks::<6>().0.iter().all(|f| f[..4].iter().all(|s| *s == 0.0)));
        assert!((data[data.len() - 2] - 0.25).abs() < 1e-4);
        assert!((data[data.len() - 1] + 0.5).abs() < 1e-4);
    }

    #[test]
    fn cue_clock_renders_stereo_and_recovers_without_touching_master() {
        let health = Arc::new(OutputHealth::default());
        health.master.frames.store(256, Ordering::Relaxed);
        let (mut tx, rx) = rtrb::RingBuffer::new(96000);
        let mut cue = CueOutput::new(rx, health.clone(), Instant::now(), 48000, 0.0);
        let mut out = vec![0.0; 512];
        for _ in 0..2000 {
            tx.push(CueFrame {
                samples: [0.25, -0.5],
                master_ns: 1,
            })
            .unwrap();
        }
        for _ in 0..8 {
            cue.render_at(&mut out, Some(30_000_000));
        }
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(health.underruns.load(Ordering::Relaxed) > 0);
        for _ in 0..20 {
            cue.render_at(&mut out, None);
        }
        assert!(out.iter().all(|s| s.abs() < 1e-6));
        for _ in 0..2000 {
            tx.push(CueFrame {
                samples: [0.25, -0.5],
                master_ns: 1,
            })
            .unwrap();
        }
        cue.render_at(&mut out, None);
        assert!((out[510] - 0.25).abs() < 1e-4);
        assert!((out[511] + 0.5).abs() < 1e-4);
        assert_eq!(health.offset_us.load(Ordering::Relaxed), i32::MIN);
        health.reset.store(true, Ordering::Relaxed);
        cue.render_at(&mut out, None);
        assert!(out.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn independently_scheduled_callbacks_with_different_sizes_and_jitter() {
        for ppm in [-1000.0, 1000.0] {
            let health = Arc::new(OutputHealth::default());
            let (mut tx, rx) = rtrb::RingBuffer::new(96000);
            let mut cue = CueOutput::new(rx, health.clone(), Instant::now(), 48000, 0.0);
            let mut master_time = 0.0;
            let mut cue_time = 0.001;
            let mut master_frames = 0u64;
            let mut master_blocks = 0;
            let mut cue_blocks = 0;
            let mut data = [0.0; 1024];
            while master_time < 30.0 || cue_time < 30.0 {
                if master_time <= cue_time {
                    let n = [128, 512, 256, 512][master_blocks % 4];
                    health.master.frames.store(n, Ordering::Relaxed);
                    for f in 0..n {
                        let s = (2.0 * std::f64::consts::PI * 1000.0 * (master_frames + f as u64) as f64 / 48000.0).sin() as f32 * 0.1;
                        tx.push(CueFrame {
                            samples: [s, -s],
                            master_ns: ((1.0 + master_time + 0.01) * 1e9) as u64 + f as u64 * 1_000_000_000 / 48000,
                        })
                        .unwrap();
                    }
                    master_frames += n as u64;
                    master_time += n as f64 / 48000.0 + if master_blocks % 2 == 0 { 0.0002 } else { -0.0002 };
                    master_blocks += 1;
                } else {
                    let n = [256, 128, 512, 128][cue_blocks % 4];
                    let out = &mut data[..n * 2];
                    cue.render_at(out, Some(((1.0 + cue_time + 0.02) * 1e9) as u64));
                    for frame in out.as_chunks::<2>().0 {
                        assert!(frame[0].is_finite() && frame[0].abs() < 0.11);
                        assert_eq!(frame[0], -frame[1]);
                    }
                    cue_time += n as f64 / (48000.0 * (1.0 + ppm * 1e-6)) + if cue_blocks % 2 == 0 { -0.00015 } else { 0.00015 };
                    cue_blocks += 1;
                }
            }
            assert_eq!(health.underruns.load(Ordering::Relaxed), 0);
            assert_eq!(health.overflows.load(Ordering::Relaxed), 0);
            assert!(cue.input.slots() < 4096);
            assert!(health.master_delay.load(Ordering::Relaxed) > 0);
        }
    }
}
