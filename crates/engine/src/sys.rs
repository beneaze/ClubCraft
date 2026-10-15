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
