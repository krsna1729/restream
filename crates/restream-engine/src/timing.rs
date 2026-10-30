//! Per-thread CPU readings for the media threads' own utilisation figures.
//!
//! [`thread_cpu_ns`] reads the calling thread's CPU clock
//! (`CLOCK_THREAD_CPUTIME_ID`, a system call of about 0.2 µs) for the egress
//! shards' utilisation and the media executor's per-poll CPU. [`thread_usage`] reads `getrusage(RUSAGE_THREAD)` (about
//! 0.2 µs): the user/system split, which the kernel apportions from tick
//! samples, and exact minor/major fault counts. Wall-clock intervals use
//! `std::time::Instant`.

/// CPU time the calling thread has consumed.
pub(crate) fn thread_cpu_ns() -> u64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    (ts.tv_sec.max(0) as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec.max(0) as u64)
}

/// The calling thread's cumulative user and system CPU and page faults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUsage {
    pub user_us: u64,
    pub sys_us: u64,
    pub minor_faults: u64,
    pub major_faults: u64,
}

impl ThreadUsage {
    /// What the thread consumed since `earlier`, clamped at zero per field.
    pub(crate) fn since(self, earlier: Self) -> Self {
        Self {
            user_us: self.user_us.saturating_sub(earlier.user_us),
            sys_us: self.sys_us.saturating_sub(earlier.sys_us),
            minor_faults: self.minor_faults.saturating_sub(earlier.minor_faults),
            major_faults: self.major_faults.saturating_sub(earlier.major_faults),
        }
    }
}

/// `getrusage(RUSAGE_THREAD)` for the calling thread. All zero if the call
/// fails.
pub(crate) fn thread_usage() -> ThreadUsage {
    // SAFETY: `rusage` is plain old data; all-zero is a valid value.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `usage` is a valid out-pointer for the call.
    if unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut usage) } != 0 {
        return ThreadUsage::default();
    }
    let micros = |tv: libc::timeval| {
        (tv.tv_sec.max(0) as u64)
            .saturating_mul(1_000_000)
            .saturating_add(tv.tv_usec.max(0) as u64)
    };
    ThreadUsage {
        user_us: micros(usage.ru_utime),
        sys_us: micros(usage.ru_stime),
        minor_faults: usage.ru_minflt.max(0) as u64,
        major_faults: usage.ru_majflt.max(0) as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_cpu_advances_with_work_on_this_thread() {
        let before = thread_cpu_ns();
        let start = std::time::Instant::now();
        let mut x = 0u64;
        while start.elapsed() < std::time::Duration::from_millis(20) {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        let spent = thread_cpu_ns().saturating_sub(before);
        assert!(
            spent >= 5_000_000,
            "20 ms of spinning read as {spent} ns of CPU"
        );
    }

    #[test]
    fn thread_usage_since_never_goes_negative() {
        let later = thread_usage();
        let earlier = ThreadUsage {
            user_us: later.user_us + 1,
            ..later
        };
        assert_eq!(later.since(earlier).user_us, 0);
    }
}
