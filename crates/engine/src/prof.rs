//! Section timers for the audio callback (diagnostics; audio thread only).

use std::cell::Cell;
use std::time::Instant;

pub const SECTIONS: usize = 5;
/// Section names: stretcher restart, stretcher process, source reads, crossfade tails,
/// transpose changes.
pub const NAMES: [&str; SECTIONS] = ["stretch restart", "stretch process", "source read", "crossfade tail", "transpose"];

thread_local! {
    static ACC: [Cell<u32>; SECTIONS] = const { [const { Cell::new(0) }; SECTIONS] };
}

/// Time `f` into section `k` (µs, accumulated per callback).
#[inline]
pub fn time<R>(k: usize, f: impl FnOnce() -> R) -> R {
    let t = Instant::now();
    let r = f();
    let us = t.elapsed().as_micros() as u32;
    ACC.with(|a| a[k].set(a[k].get().saturating_add(us)));
    r
}

/// Take (and reset) the accumulated times of this callback.
pub fn take() -> [u32; SECTIONS] {
    ACC.with(|a| std::array::from_fn(|k| a[k].replace(0)))
}
