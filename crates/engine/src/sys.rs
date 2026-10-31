//! The engine's OS calls, kept in one place: everything else in the crate is safe code.

/// Page faults of the process so far (minor faults include pages macOS decompresses from its
/// memory compressor).
pub fn page_faults() -> i64 {
    #[cfg(unix)]
    {
        // SAFETY: `rusage` is plain old data, so all zeros is a valid value; getrusage only
        // writes into the struct we pass.
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: as above; the pointer is to a live local.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } == 0 {
            return u.ru_majflt as i64 + u.ru_minflt as i64;
        }
    }
    0
}

/// Keep `v` in RAM (no swapping or compression). Returns whether the system allowed it.
pub fn lock<T>(v: &[T]) -> bool {
    if v.is_empty() {
        return true;
    }
    #[cfg(unix)]
    {
        // SAFETY: the range is a live allocation borrowed for the call; mlock only changes its
        // residency, never its contents.
        unsafe { libc::mlock(v.as_ptr().cast(), std::mem::size_of_val(v)) == 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Undo [`lock`].
pub fn unlock<T>(v: &[T]) {
    if v.is_empty() {
        return;
    }
    #[cfg(unix)]
    {
        // SAFETY: as in `lock`.
        unsafe {
            libc::munlock(v.as_ptr().cast(), std::mem::size_of_val(v));
        }
    }
}

/// Give the calling thread a lower CPU priority, for background work (stem separation,
/// analysis) that must not starve the audio thread; threads it starts later, like ONNX
/// Runtime's pool, inherit it. On Linux the audio thread has no real-time priority (cpal only
/// asks for it on raw ALSA devices, not through PipeWire's or PulseAudio's ALSA plugin), so a
/// separation on every core caused underruns. Elsewhere the audio thread is real-time already.
pub fn background_priority() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: setpriority takes no pointers; on Linux `who = 0` is the calling thread.
        if unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) } != 0 {
            log::warn!("lower thread priority: {}", std::io::Error::last_os_error());
        }
    }
}
