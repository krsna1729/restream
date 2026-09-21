//! The sender thread: one pinned CPU, a pause barrier for interval-aligned
//! snapshots, and the counters every arm shares.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::super::peer_state::{ThreadCpu, pin_to_cpuset, thread_cpus_allowed_list};
use super::arms::{run_compio, run_compio_pipeline, run_io_uring, run_sendto};
use super::config::*;

#[derive(Default)]
pub(crate) struct SenderCounters {
    pub(crate) submitted: AtomicU64,
    pub(crate) completed: AtomicU64,
    pub(crate) ring_enters: AtomicU64,
    pub(crate) errors: AtomicU64,
    pub(crate) max_in_flight: AtomicU64,
    pub(crate) batches: AtomicU64,
}

pub(crate) struct SenderHandles {
    pub(crate) tid: AtomicI32,
    pub(crate) stop: Arc<AtomicBool>,
    /// Requested pause: the sender parks before its next datagram so the
    /// harness can snapshot both sides over one interval.
    pub(crate) pause: Arc<AtomicBool>,
    /// Pause acknowledged: the sender is parked and its counters are stable.
    pub(crate) paused: Arc<AtomicBool>,
    /// The sender's own CPU time (user, system, switches) sampled as it parks.
    /// Plain atomics: the reader only reads while the sender is parked, so the
    /// four values always belong to the same sample.
    pub(crate) paused_user_micros: AtomicU64,
    pub(crate) paused_system_micros: AtomicU64,
    pub(crate) paused_voluntary: AtomicU64,
    pub(crate) paused_involuntary: AtomicU64,
    pub(crate) counters: Arc<SenderCounters>,
}

impl SenderHandles {
    pub(crate) fn new() -> Self {
        Self {
            tid: AtomicI32::new(0),
            stop: Arc::new(AtomicBool::new(false)),
            pause: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            paused_user_micros: AtomicU64::new(0),
            paused_system_micros: AtomicU64::new(0),
            paused_voluntary: AtomicU64::new(0),
            paused_involuntary: AtomicU64::new(0),
            counters: Arc::new(SenderCounters::default()),
        }
    }

    pub(crate) fn completed(&self) -> u64 {
        self.counters.completed.load(Ordering::Relaxed)
    }

    /// Park the sender and take its own on-CPU/off-CPU sample. Called from the
    /// hot loop, so the fast path is one relaxed load.
    pub(crate) fn wait_if_paused(&self) {
        if !self.pause.load(Ordering::Acquire) {
            return;
        }
        let tid = self.tid.load(Ordering::Relaxed);
        if let Some(cpu) = super::peer_state::thread_cpu(tid) {
            self.paused_user_micros
                .store((cpu.user_secs * 1e6) as u64, Ordering::Relaxed);
            self.paused_system_micros
                .store((cpu.system_secs * 1e6) as u64, Ordering::Relaxed);
            self.paused_voluntary
                .store(cpu.voluntary_switches, Ordering::Relaxed);
            self.paused_involuntary
                .store(cpu.involuntary_switches, Ordering::Relaxed);
        }
        self.paused.store(true, Ordering::Release);
        while self.pause.load(Ordering::Acquire) && !self.stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_micros(200));
        }
        self.paused.store(false, Ordering::Release);
    }

    /// The sample taken when the sender last parked. Only meaningful while it
    /// is still parked.
    pub(crate) fn cpu_sample(&self) -> ThreadCpu {
        ThreadCpu {
            user_secs: self.paused_user_micros.load(Ordering::Relaxed) as f64 / 1e6,
            system_secs: self.paused_system_micros.load(Ordering::Relaxed) as f64 / 1e6,
            voluntary_switches: self.paused_voluntary.load(Ordering::Relaxed),
            involuntary_switches: self.paused_involuntary.load(Ordering::Relaxed),
        }
    }
}

/// Park the sender, snapshot, release: `pause` plus a bounded wait for the
/// acknowledgement, so a wedged sender is an error rather than a hang.
pub(crate) async fn pause_sender(handles: &SenderHandles) -> Result<(), String> {
    handles.pause.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !handles.paused.load(Ordering::Acquire) {
        if Instant::now() > deadline {
            return Err("sender did not acknowledge the pause within 5s".to_string());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    Ok(())
}

pub(crate) async fn resume_sender(handles: &SenderHandles) -> Result<(), String> {
    handles.pause.store(false, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(5);
    while handles.paused.load(Ordering::Acquire) {
        if Instant::now() > deadline {
            return Err("sender did not leave the pause within 5s".to_string());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    Ok(())
}

/// What the sender thread reports back once it has stopped: the mask it
/// actually observed and how the variant ended. Both are read after `join`, so
/// they travel as the thread's return value rather than through a lock.
pub(crate) struct SenderReport {
    pub(crate) observed_mask: Option<String>,
    pub(crate) outcome: Result<(), String>,
}

/// The sender thread: pin to exactly one CPU, record the observed mask, then
/// run the configured variant until asked to stop.
pub(crate) fn sender_thread(
    config: Arc<SubstrateConfig>,
    payload: Bytes,
    handles: Arc<SenderHandles>,
) -> SenderReport {
    let outcome = (|| -> Result<(), String> {
        pin_to_cpuset(&config.sender_cpus)?;
        handles
            .tid
            .store(super::peer_state::current_thread_id(), Ordering::Relaxed);
        match config.variant {
            Variant::Compio => run_compio(&config, payload, &handles),
            Variant::CompioPipeline => run_compio_pipeline(&config, payload, &handles),
            Variant::IoUring => run_io_uring(&config, payload, &handles),
            Variant::Sendto => run_sendto(&config, payload, &handles),
        }
    })();
    SenderReport {
        observed_mask: thread_cpus_allowed_list(handles.tid.load(Ordering::Relaxed)),
        outcome,
    }
}
