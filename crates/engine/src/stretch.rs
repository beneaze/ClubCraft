//! Safe wrapper around the vendored Signalsmith Stretch (interleaved stereo).
//!
//! Compiled from `vendor/` by `build.rs` with `splitComputation` support and, on macOS, Apple
//! Accelerate FFTs. Processing allocates nothing (all buffers are sized in [`Stretcher::new`]).

use std::os::raw::{c_double, c_float, c_int, c_void};

extern "C" {
    fn cc_stretch_create(channels: c_int, block: c_int, interval: c_int, split: c_int) -> *mut c_void;
    fn cc_stretch_destroy(h: *mut c_void);
    fn cc_stretch_reset(h: *mut c_void);
    fn cc_stretch_input_latency(h: *mut c_void) -> c_int;
    fn cc_stretch_output_latency(h: *mut c_void) -> c_int;
    fn cc_stretch_set_transpose(h: *mut c_void, factor: c_float);
    fn cc_stretch_seek(h: *mut c_void, input: *const c_float, frames: c_int, rate: c_double);
    fn cc_stretch_process(h: *mut c_void, input: *const c_float, in_frames: c_int, output: *mut c_float, out_frames: c_int);
}

/// Time stretcher / pitch shifter for interleaved audio.
pub struct Stretcher {
    h: *mut c_void,
    channels: usize,
}

// SAFETY: the C++ object is only accessed through `&mut self` (or `&self` for the latency
// getters, which only read).
unsafe impl Send for Stretcher {}

impl Stretcher {
    /// `block` / `interval` in samples; `split` spreads the spectral computation evenly across
    /// calls (adds one interval of output latency, avoids CPU spikes).
    /// If the C++ side cannot allocate, the stretcher stays silent instead of crashing.
    pub fn new(channels: usize, block: usize, interval: usize, split: bool) -> Self {
        // SAFETY: plain values in; the returned handle is owned by this struct.
        let h = unsafe { cc_stretch_create(channels as c_int, block as c_int, interval as c_int, split as c_int) };
        if h.is_null() {
            log::error!("time stretcher: allocation failed");
        }
        Stretcher { h, channels: channels.max(1) }
    }

    pub fn reset(&mut self) {
        if !self.h.is_null() {
            // SAFETY: `h` is a live handle from `cc_stretch_create`.
            unsafe { cc_stretch_reset(self.h) }
        }
    }

    pub fn input_latency(&self) -> usize {
        if self.h.is_null() {
            return 0;
        }
        // SAFETY: as in `reset`.
        unsafe { cc_stretch_input_latency(self.h) as usize }
    }

    pub fn output_latency(&self) -> usize {
        if self.h.is_null() {
            return 0;
        }
        // SAFETY: as in `reset`.
        unsafe { cc_stretch_output_latency(self.h) as usize }
    }

    /// Frequency multiplier (1 = unchanged, 2 = up an octave).
    pub fn set_transpose_factor(&mut self, factor: f32) {
        if !self.h.is_null() {
            // SAFETY: as in `reset`.
            unsafe { cc_stretch_set_transpose(self.h, factor) }
        }
    }

    /// Provide pre-roll input without advancing the stream position.
    pub fn seek(&mut self, input: &[f32], rate: f64) {
        let frames = input.len() / self.channels;
        if !self.h.is_null() {
            // SAFETY: live handle; the C++ side reads `frames * channels` samples, all inside `input`.
            unsafe { cc_stretch_seek(self.h, input.as_ptr(), frames as c_int, rate) }
        }
    }

    /// Consume `input.len() / ch` frames, produce `output.len() / ch` frames.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let fi = input.len() / self.channels;
        let fo = output.len() / self.channels;
        if self.h.is_null() {
            output.fill(0.0);
            return;
        }
        // SAFETY: live handle; it reads `fi` and writes `fo` interleaved frames, all inside the
        // two slices.
        unsafe { cc_stretch_process(self.h, input.as_ptr(), fi as c_int, output.as_mut_ptr(), fo as c_int) }
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        if !self.h.is_null() {
            // SAFETY: the handle is live and dropped exactly once.
            unsafe { cc_stretch_destroy(self.h) }
        }
    }
}
