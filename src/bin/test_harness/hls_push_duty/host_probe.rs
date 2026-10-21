//! Independent clock-gap samples beside a `hls-push-duty` rated window.
//!
//! The probe performs no application work between clock reads. Its gaps
//! provide scheduling/timing context, not causal attribution for application
//! polls: they run on different threads and need not overlap. Executor
//! maxima are lifetime values; this report covers only the rated window.
//!
//! Enabled with `--host-probe`. The nominal 1 ms spin / 4 ms sleep cycle
//! adds about 20% of one core and misses gaps outside its spinning windows.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A wall gap at or above this is included in the diagnostic report.
const GAP_NS: u64 = 1_000_000;

/// Batch clock reads around a small spin loop instead of reading every turn.
const LOOPS_PER_SAMPLE: u32 = 5_000;

/// What the probe saw, and the load it added.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HostProbeReport {
    /// The spinning window and the sleep after it, per cycle.
    pub spin_us: u64,
    pub sleep_us: u64,
    /// Clock pairs read while spinning.
    pub samples: u64,
    /// Spinning samples whose wall delta was at least 1 ms.
    pub gaps_over_1ms: u64,
    /// Samples whose thread-CPU delta was at least half the wall delta.
    /// This describes clock behavior; it does not identify the gap's cause.
    pub runnable_gaps: u64,
    pub max_wall_gap_us: u64,
    pub max_cpu_gap_us: u64,
    /// The longest overshoot of a requested sleep: a wall-only stall (the
    /// guest knew the thread was parked), reported for context.
    pub wake_delay_max_us: u64,
}

impl HostProbeReport {
    /// Record one spinning sample. Clamp CPU to wall time to limit skew
    /// from reading the CPU clock after the wall clock.
    fn record(&mut self, wall_ns: u64, cpu_ns: u64) {
        self.samples = self.samples.saturating_add(1);
        if wall_ns < GAP_NS {
            return;
        }
        let cpu_ns = cpu_ns.min(wall_ns);
        self.gaps_over_1ms = self.gaps_over_1ms.saturating_add(1);
        if cpu_ns.saturating_mul(2) >= wall_ns {
            self.runnable_gaps = self.runnable_gaps.saturating_add(1);
        }
        self.max_wall_gap_us = self.max_wall_gap_us.max(wall_ns / 1_000);
        self.max_cpu_gap_us = self.max_cpu_gap_us.max(cpu_ns / 1_000);
    }
}

/// The probe thread. Dropping it stops and joins the thread, so an early
/// return from an arm cannot leave it behind.
pub(crate) struct HostProbe {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<HostProbeReport, String>>>,
}

impl HostProbe {
    /// Start the probe. Failure to start an explicitly requested probe is a
    /// measurement error, not an empty successful report.
    pub(crate) fn start(spin: Duration, sleep: Duration) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = thread::Builder::new()
            .name("host-probe".to_string())
            .spawn(move || run(thread_stop, spin, sleep, thread_cpu_ns))
            .map_err(|error| format!("start host probe: {error}"))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// Stop and join the probe. Clock errors and thread panics invalidate
    /// the requested measurement instead of yielding zero counters.
    pub(crate) fn stop(mut self) -> Result<HostProbeReport, String> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .ok_or_else(|| "host probe already joined".to_string())?
            .join()
            .map_err(|_| "host probe thread panicked".to_string())?
    }
}

impl Drop for HostProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    stop: Arc<AtomicBool>,
    spin: Duration,
    sleep: Duration,
    mut cpu_clock: impl FnMut() -> io::Result<u64>,
) -> Result<HostProbeReport, String> {
    let mut report = HostProbeReport {
        spin_us: spin.as_micros() as u64,
        sleep_us: sleep.as_micros() as u64,
        ..HostProbeReport::default()
    };
    loop {
        let spin_end = Instant::now() + spin;
        let mut wall = Instant::now();
        let mut cpu = cpu_clock().map_err(|error| format!("host probe CPU clock: {error}"))?;
        while Instant::now() < spin_end {
            for _ in 0..LOOPS_PER_SAMPLE {
                std::hint::spin_loop();
            }
            let next_wall = Instant::now();
            let next_cpu = cpu_clock().map_err(|error| format!("host probe CPU clock: {error}"))?;
            report.record(
                next_wall.duration_since(wall).as_nanos() as u64,
                next_cpu.saturating_sub(cpu),
            );
            wall = next_wall;
            cpu = next_cpu;
        }
        // One spinning window always runs, so a probe stopped immediately
        // still has its samples; stopping costs at most one sleep.
        if stop.load(Ordering::Acquire) {
            break;
        }
        let wake_at = Instant::now() + sleep;
        thread::sleep(sleep);
        let delay = Instant::now().duration_since(wake_at).as_micros() as u64;
        report.wake_delay_max_us = report.wake_delay_max_us.max(delay);
        if stop.load(Ordering::Acquire) {
            break;
        }
    }
    Ok(report)
}

/// The calling thread's own CPU time, the clock the executor polls use.
fn thread_cpu_ns() -> io::Result<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer for the call; the clock id is a
    // constant supported on Linux.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((ts.tv_sec.max(0) as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec.max(0) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_gaps_at_or_above_a_millisecond_are_recorded() {
        let mut report = HostProbeReport::default();
        report.record(999_999, 900_000);
        assert_eq!((report.samples, report.gaps_over_1ms), (1, 0));
        report.record(1_000_000, 1_000_000);
        assert_eq!((report.gaps_over_1ms, report.runnable_gaps), (1, 1));
        assert_eq!(
            (report.max_wall_gap_us, report.max_cpu_gap_us),
            (1_000, 1_000)
        );
    }

    #[test]
    fn a_wall_gap_can_have_little_thread_cpu() {
        let mut report = HostProbeReport::default();
        report.record(60_000_000, 40_000);
        assert_eq!(report.gaps_over_1ms, 1);
        assert_eq!(report.runnable_gaps, 0);
        assert_eq!(report.max_wall_gap_us, 60_000);
        assert_eq!(report.max_cpu_gap_us, 40);
    }

    #[test]
    fn cpu_deltas_are_clamped_to_the_wall_interval() {
        let mut report = HostProbeReport::default();
        report.record(60_718_000, 60_345_000);
        assert_eq!(report.runnable_gaps, 1);
        assert_eq!(report.max_cpu_gap_us, 60_345);
        // Clock skew must not let the CPU gap exceed the wall gap.
        let mut skew = HostProbeReport::default();
        skew.record(2_000_000, 9_000_000);
        assert_eq!((skew.max_wall_gap_us, skew.max_cpu_gap_us), (2_000, 2_000));
    }

    #[test]
    fn a_cpu_clock_error_invalidates_the_probe() {
        let error = run(
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(1),
            Duration::from_millis(4),
            || Err(io::Error::other("unavailable clock")),
        )
        .unwrap_err();
        assert!(error.contains("CPU clock"), "{error}");
    }

    #[test]
    fn a_panicked_probe_invalidates_the_measurement() {
        let thread = thread::spawn(|| -> Result<HostProbeReport, String> {
            // Bypass the panic hook: this is an expected thread failure.
            std::panic::resume_unwind(Box::new("injected probe panic"));
        });
        let probe = HostProbe {
            stop: Arc::new(AtomicBool::new(false)),
            thread: Some(thread),
        };
        assert!(probe.stop().unwrap_err().contains("panicked"));
    }
}
