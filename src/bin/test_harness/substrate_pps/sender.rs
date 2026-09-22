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
    /// Stage B only: thread CPU spent queuing pre-materialized datagrams. Kept
    /// out of the Owner-drive denominator by construction.
    pub(crate) injection_cpu_micros: AtomicU64,
    /// Stage B only: thread CPU spent driving `Owner::service` and reaping.
    pub(crate) owner_drive_cpu_micros: AtomicU64,
}

/// Accumulated production-transport accounting for a Stage-B run: every delta
/// the Owner reports, so the run's validity is provenance rather than
/// `injected == completed` bookkeeping.
/// Accumulated production-transport accounting. `#[allow(dead_code)]` because
/// only the `wi3-owner-bench` arm writes it, while every build reads it in the
/// artifact path.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub(crate) struct ServiceTotals {
    pub(crate) visits: u64,
    pub(crate) actions: u64,
    pub(crate) maintenance_actions: u64,
    pub(crate) submitted: u64,
    pub(crate) completed_ok: u64,
    pub(crate) completions_reaped: u64,
    pub(crate) short_sends: u64,
    pub(crate) failed_sends: u64,
    pub(crate) transient_failures: u64,
    pub(crate) peer_local_failures: u64,
    pub(crate) protocol_output_failures: u64,
    pub(crate) budget_exhausted: u64,
    pub(crate) in_flight_hwm: u64,
    pub(crate) tx_pool_free_min: u64,
}

#[allow(dead_code)]
impl ServiceTotals {
    fn observe_in_flight(&mut self, in_flight: usize, pool_free: usize) {
        self.in_flight_hwm = self.in_flight_hwm.max(in_flight as u64);
        self.tx_pool_free_min = if self.tx_pool_free_min == 0 {
            pool_free as u64
        } else {
            self.tx_pool_free_min.min(pool_free as u64)
        };
    }

    pub(crate) fn absorb(
        &mut self,
        report: &srt_transport::compio::OwnerServiceReport,
        counters: &SenderCounters,
    ) {
        self.visits += 1;
        self.actions += report.actions as u64;
        self.maintenance_actions += report.maintenance_actions as u64;
        self.submitted += report.tx_packets_submitted as u64;
        self.completed_ok += report.tx_completed_ok as u64;
        self.completions_reaped += report.completions_reaped as u64;
        self.short_sends += report.tx_short_sends as u64;
        self.failed_sends += report.tx_failed_sends as u64;
        self.transient_failures += report.tx_transient_failures as u64;
        self.peer_local_failures += report.tx_peer_local_failures as u64;
        self.protocol_output_failures += report.protocol_output_failures as u64;
        if report.budget_exhausted {
            self.budget_exhausted += 1;
        }
        counters
            .batches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.observe_in_flight(report.tx_in_flight, report.tx_pool_free);
    }

    /// Quiescence drains account into the totals without touching per-iteration
    /// counters, so they do not inflate the window's batch count.
    pub(crate) fn absorb_report_only(
        &mut self,
        report: &srt_transport::compio::OwnerServiceReport,
    ) {
        self.actions += report.actions as u64;
        self.maintenance_actions += report.maintenance_actions as u64;
        self.submitted += report.tx_packets_submitted as u64;
        self.completed_ok += report.tx_completed_ok as u64;
        self.completions_reaped += report.completions_reaped as u64;
        self.short_sends += report.tx_short_sends as u64;
        self.failed_sends += report.tx_failed_sends as u64;
        self.transient_failures += report.tx_transient_failures as u64;
        self.peer_local_failures += report.tx_peer_local_failures as u64;
        self.protocol_output_failures += report.protocol_output_failures as u64;
        if report.budget_exhausted {
            self.budget_exhausted += 1;
        }
        self.observe_in_flight(report.tx_in_flight, report.tx_pool_free);
    }

    pub(crate) fn submitted(&self) -> u64 {
        self.submitted
    }

    pub(crate) fn completed_ok(&self) -> u64 {
        self.completed_ok
    }

    pub(crate) fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "serviceVisits": self.visits,
            "actions": self.actions,
            "maintenanceActions": self.maintenance_actions,
            "txSubmitted": self.submitted,
            "txCompletedOk": self.completed_ok,
            "completionsReaped": self.completions_reaped,
            "txShortSends": self.short_sends,
            "txFailedSends": self.failed_sends,
            "txTransientFailures": self.transient_failures,
            "txPeerLocalFailures": self.peer_local_failures,
            "protocolOutputFailures": self.protocol_output_failures,
            "budgetExhaustedVisits": self.budget_exhausted,
            "inFlightHwm": self.in_flight_hwm,
            "txPoolFreeMin": self.tx_pool_free_min,
        })
    }
}

pub(crate) struct SenderHandles {
    pub(crate) tid: AtomicI32,
    /// Stage B only: the finished Owner accumulation, attached to the report by
    /// `sender_thread` after the arm returns.
    pub(crate) finished_totals: std::sync::Mutex<Option<ServiceTotals>>,
    pub(crate) stop: Arc<AtomicBool>,
    /// Requested pause: the sender parks before its next datagram so the
    /// harness can snapshot both sides over one interval.
    pub(crate) pause: Arc<AtomicBool>,
    /// Pause acknowledged: the sender is parked and its counters are stable.
    pub(crate) paused: Arc<AtomicBool>,
    /// The sender thread has exited (either normally or via error).
    pub(crate) exited: Arc<AtomicBool>,
    /// Error that caused the sender thread to exit early.
    pub(crate) exit_error: Arc<std::sync::Mutex<Option<String>>>,
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
            finished_totals: std::sync::Mutex::new(None),
            stop: Arc::new(AtomicBool::new(false)),
            pause: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            exited: Arc::new(AtomicBool::new(false)),
            exit_error: Arc::new(std::sync::Mutex::new(None)),
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

    /// The current Stage-B scope readings, for window differencing.
    pub(crate) fn cpu_scopes(&self) -> CpuScopes {
        CpuScopes {
            injection: self.counters.injection_cpu_micros.load(Ordering::Relaxed),
            drive: self.counters.owner_drive_cpu_micros.load(Ordering::Relaxed),
        }
    }

    /// Whether a snapshot boundary has been requested. The hot path is one
    /// relaxed load; the arm must then drain its in-flight work to zero and call
    /// [`Self::acknowledge_pause`], so the boundary is quiescent rather than
    /// merely "not refilling".
    pub(crate) fn pause_requested(&self) -> bool {
        self.pause.load(Ordering::Acquire)
    }

    /// Park the sender and take its own on-CPU/off-CPU sample. Callers must have
    /// drained every already-submitted operation first: with a queue depth of 64,
    /// acknowledging at the top of the loop would leave ~63 datagrams in flight,
    /// free to cross the peer snapshot boundary and be counted on one side of the
    /// window only.
    pub(crate) fn acknowledge_pause(&self) {
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
        if handles.exited.load(Ordering::Acquire) {
            let err = handles.exit_error.lock().ok().and_then(|mut g| g.take());
            return Err(format!(
                "sender exited before acknowledging pause: {}",
                err.unwrap_or_else(|| "unknown error".to_string())
            ));
        }
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
        if handles.exited.load(Ordering::Acquire) {
            let err = handles.exit_error.lock().ok().and_then(|mut g| g.take());
            return Err(format!(
                "sender exited during pause: {}",
                err.unwrap_or_else(|| "unknown error".to_string())
            ));
        }
        if Instant::now() > deadline {
            return Err("sender did not leave the pause within 5s".to_string());
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    Ok(())
}

/// Window-baseline readings of the Stage-B CPU scopes.
#[derive(Clone, Copy, Default)]
pub(crate) struct CpuScopes {
    pub(crate) injection: u64,
    pub(crate) drive: u64,
}

/// What the sender thread reports back once it has stopped: the mask it
/// actually observed and how the variant ended. Both are read after `join`, so
/// they travel as the thread's return value rather than through a lock.
pub(crate) struct SenderReport {
    pub(crate) observed_mask: Option<String>,
    /// Stage B only: the finished Owner accumulation, attached by
    /// `sender_thread` from the shared handles.
    pub(crate) owner_totals: std::sync::Mutex<Option<ServiceTotals>>,
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
            #[cfg(feature = "wi3-owner-bench")]
            Variant::OwnerTx => super::owner_tx::run_owner_tx(&config, payload, &handles),
            #[cfg(not(feature = "wi3-owner-bench"))]
            Variant::OwnerTx => Err(
                "SUBSTRATE_VARIANT=owner-tx needs a build with --features wi3-owner-bench: the \
                 Stage-B arm uses benchmark-only srt-transport internals"
                    .to_string(),
            ),
        }
    })();
    if let Err(err) = &outcome
        && let Ok(mut guard) = handles.exit_error.lock()
    {
        *guard = Some(err.clone());
    }
    handles.exited.store(true, Ordering::Release);
    // report carries provenance rather than bookkeeping.
    let finished = handles
        .finished_totals
        .lock()
        .map(|mut guard| guard.take())
        .unwrap_or(None);
    SenderReport {
        observed_mask: thread_cpus_allowed_list(handles.tid.load(Ordering::Relaxed)),
        owner_totals: std::sync::Mutex::new(finished),
        outcome,
    }
}

impl SenderReport {
    pub(crate) fn take_owner_totals(&self) -> Option<ServiceTotals> {
        self.owner_totals
            .lock()
            .map(|mut guard| guard.take())
            .unwrap_or(None)
    }
}
