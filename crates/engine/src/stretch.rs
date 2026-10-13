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

// SAFETY: the C++ object is only accessed through `&mut self`.
unsafe impl Send for Stretcher {}

impl Stretcher {
    /// `block` / `interval` in samples; `split` spreads the spectral computation evenly across
    /// calls (adds one interval of output latency, avoids CPU spikes).
    pub fn new(channels: usize, block: usize, interval: usize, split: bool) -> Self {
        let h = unsafe { cc_stretch_create(channels as c_int, block as c_int, interval as c_int, split as c_int) };
        assert!(!h.is_null());
        Stretcher { h, channels }
    }

    pub fn reset(&mut self) {
        unsafe { cc_stretch_reset(self.h) }
    }

    pub fn input_latency(&self) -> usize {
        unsafe { cc_stretch_input_latency(self.h) as usize }
    }

    pub fn output_latency(&self) -> usize {
        unsafe { cc_stretch_output_latency(self.h) as usize }
    }

    /// Frequency multiplier (1 = unchanged, 2 = up an octave).
    pub fn set_transpose_factor(&mut self, factor: f32) {
        unsafe { cc_stretch_set_transpose(self.h, factor) }
    }

    /// Provide pre-roll input without advancing the stream position.
    pub fn seek(&mut self, input: &[f32], rate: f64) {
        let frames = input.len() / self.channels;
        unsafe { cc_stretch_seek(self.h, input.as_ptr(), frames as c_int, rate) }
    }

    /// Consume `input.len() / ch` frames, produce `output.len() / ch` frames.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let fi = input.len() / self.channels;
        let fo = output.len() / self.channels;
        unsafe { cc_stretch_process(self.h, input.as_ptr(), fi as c_int, output.as_mut_ptr(), fo as c_int) }
    }
}

impl Drop for Stretcher {
    fn drop(&mut self) {
        unsafe { cc_stretch_destroy(self.h) }
    }
}
