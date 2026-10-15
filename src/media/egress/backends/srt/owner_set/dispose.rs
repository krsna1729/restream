//! Disposing a family Owner: the record that keeps the family closed and
//! visible, plus the teardown that releases what a faulted Owner can no longer
//! reclaim. A child of `owner_set`, so it reaches the shard's Owner slots and
//! counters directly instead of adding accessors.

use super::{AddressFamily, FamilyOwner, OWNER_SHUTDOWN_RESERVE, SrtOwners};
use crate::media::egress::metrics::OwnerFamilyMetrics;
use srt_transport::compio::OwnerRxMode;
use std::time::{Duration, Instant};

/// Bound for a retried teardown attempt. Short on purpose: the attempt still
/// drives the runtime, but it never holds the shard's sibling family for long.
const DISPOSAL_RETRY_BOUND: Duration = Duration::from_millis(25);
/// Spacing between retried attempts, doubling per attempt up to the ceiling so
/// a teardown that cannot reach quiescence keeps its runtime driven without
/// keeping the shard busy.
const DISPOSAL_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const DISPOSAL_RETRY_INTERVAL_MAX: Duration = Duration::from_millis(250);

/// What a closed family records: the fault text that keeps admission refused,
/// and the last metrics row so the fault and its final counters stay visible
/// after the Owner is gone.
pub(super) struct FamilyClosure {
    pub(super) fault: String,
    pub(super) metrics: OwnerFamilyMetrics,
}

/// When the next teardown attempt is due. Spacing doubles per miss from
/// [`DISPOSAL_RETRY_INTERVAL`] up to [`DISPOSAL_RETRY_INTERVAL_MAX`]: a
/// teardown that cannot reach quiescence keeps its runtime driven without
/// keeping the shard busy.
#[derive(Debug)]
struct DisposalSchedule {
    attempts: u32,
    next_attempt: Instant,
}

impl DisposalSchedule {
    fn after_first_miss() -> Self {
        Self {
            attempts: 0,
            next_attempt: Instant::now() + DISPOSAL_RETRY_INTERVAL,
        }
    }

    fn due(&self, now: Instant) -> bool {
        self.next_attempt <= now
    }

    fn next_attempt(&self) -> Instant {
        self.next_attempt
    }

    /// Count one miss and schedule the next attempt.
    fn bump(&mut self, now: Instant) {
        self.attempts = self.attempts.saturating_add(1);
        let spacing = DISPOSAL_RETRY_INTERVAL
            .saturating_mul(1u32 << self.attempts.min(5))
            .min(DISPOSAL_RETRY_INTERVAL_MAX);
        self.next_attempt = now + spacing;
    }
}

/// A closed family whose Owner teardown did not reach quiescence on its first
/// attempt. It is never serviced as a live Owner, never reports runnable work
/// and never admits a caller: it is kept only so the shard keeps driving its
/// runtime and retrying [`srt_transport::compio::Owner::shutdown_and_drain`]
/// until the cleanup that teardown started is discharged. Dropping the Owner
/// earlier would leave cancelled tasks and kernel-owned sends to be released
/// whenever the runtime is finally destroyed.
pub(super) struct Disposing {
    owner: FamilyOwner,
    schedule: DisposalSchedule,
}

impl Disposing {
    pub(super) fn next_attempt(&self) -> Instant {
        self.schedule.next_attempt()
    }
}

impl FamilyOwner {
    /// One low-cardinality metrics row: cumulative counters plus the gauges
    /// read at this instant. Allocation-free; the only construction site, so a
    /// disposed family's retained row has the same shape as a live one.
    pub(super) fn metrics(&self) -> OwnerFamilyMetrics {
        let owner = &self.owner;
        let counters = &self.counters;
        let tx = owner.tx_pool_snapshot();
        let pool = owner.caller_pool_stats().unwrap_or_default();
        let rx = owner.rx_stats().caller;
        OwnerFamilyMetrics {
            present: true,
            faulted: owner.fault().is_some(),
            managed_rx: owner.rx_mode() == Some(OwnerRxMode::ManagedMultishot),
            tx_capacity: tx.capacity as u32,
            tx_free: tx.free as u32,
            tx_high_water: tx.high_water as u32,
            tx_exhaustions: tx.exhaustions,
            tx_in_flight: owner.tx_in_flight() as u32,
            tx_failures_dropped: owner.tx_failures_dropped(),
            rx_packets: counters.rx_packets,
            rx_bytes: counters.rx_bytes,
            tx_packets: counters.tx_packets,
            tx_bytes: counters.tx_bytes,
            tx_class: counters.tx_class,
            tx_batching: owner.tx_batching(),
            tx_completed_ok: counters.completed_ok,
            tx_short_sends: counters.short_sends,
            tx_failed_sends: counters.failed_sends,
            tx_peer_local_failures: counters.peer_local_failures,
            tx_transient_failures: counters.transient_failures,
            protocol_output_failures: counters.protocol_output_failures,
            caller_in_flight_hwm: counters.caller_in_flight_hwm,
            caller_queued_hwm: counters.caller_queued_hwm,
            caller_queue_longest_us: counters.caller_queue_longest_us,
            service_visits: counters.service_visits,
            service_duration_sum_us: counters.service_duration_sum_us,
            service_duration_max_us: counters.service_duration_max_us,
            service_budget_exhausted: counters.budget_exhausted_visits,
            service_actions: counters.service_actions,
            maintenance_actions: counters.maintenance_actions,
            peer_group_collisions: counters.peer_group_collisions,
            caller_in_flight: pool.in_flight as u32,
            caller_queued: pool.queued as u32,
            caller_expired: pool.expired,
            caller_failed: pool.failed,
            caller_cancelled: pool.cancelled,
            rx_ring_depth: rx.map_or(0, |stats| stats.depth as u32),
            rx_ring_dropped: rx.map_or(0, |stats| stats.dropped),
            rx_buffer_exhaustions: rx.map_or(0, |stats| stats.buffer_exhaustions),
            rx_truncated: rx.map_or(0, |stats| stats.truncated),
        }
    }
}

impl SrtOwners {
    /// Dispose one faulted family Owner. Called on the fault path, right after
    /// the family's leaves and pending requests are failed.
    ///
    /// A faulted Owner never runs protocol maintenance again, so anything
    /// retired into it (the normal terminal paths, and `fail_family`'s
    /// leaves) would hold its windows and keep reporting runnable work for
    /// the life of the shard. The family is closed here, and its Owner is torn
    /// down: a quiescent teardown drops the Owner (releasing the retained
    /// protocol storage), and a teardown that misses its bound keeps the Owner
    /// in [`Disposing`] so later service visits drive it to quiescence instead
    /// of dropping cleanup the runtime still owns.
    pub(crate) fn dispose_faulted(&mut self, family: AddressFamily) {
        let index = family.index();
        let Some(mut family_owner) = self.close_family(family) else {
            return;
        };
        if self.runtime.block_on(
            family_owner
                .owner
                .shutdown_and_drain(OWNER_SHUTDOWN_RESERVE),
        ) {
            // The drop releases the retained protocol storage.
            drop(family_owner);
        } else {
            self.disposing[index] = Some(Disposing {
                owner: family_owner,
                schedule: DisposalSchedule::after_first_miss(),
            });
        }
    }

    /// Take one family out of service: record its fault text and last metrics
    /// row, purge the closing entries no Owner can flush behind any more, and
    /// hand the Owner back for disposal. Called on the fault path; a missing
    /// Owner means the family was already closed.
    fn close_family(&mut self, family: AddressFamily) -> Option<FamilyOwner> {
        self.assert_home_thread();
        let index = family.index();
        let family_owner = self.owners[index].take()?;
        let fault = family_owner
            .owner
            .fault()
            .map(|fault| format!("{fault:?}"))
            .unwrap_or_else(|| "unknown fault".to_string());
        let mut metrics = family_owner.metrics();
        metrics.faulted = true;
        tracing::warn!(
            ?family,
            fault = %fault,
            "srt egress faulted Owner closed; tearing the family down"
        );
        self.closed[index] = Some(FamilyClosure { fault, metrics });
        self.closing.retain(|entry| entry.family != family);
        Some(family_owner)
    }

    /// Test-only: park a live Owner in the pending-disposal state without a
    /// teardown attempt. A real miss needs a hung RX stop or kernel-owned sends
    /// that a harness cannot produce; everything after this point (park bound,
    /// wake, retry, release) is the production path.
    #[cfg(test)]
    pub(crate) fn force_disposal_pending(&mut self, family: AddressFamily) {
        let index = family.index();
        if let Some(family_owner) = self.close_family(family) {
            self.disposing[index] = Some(Disposing {
                owner: family_owner,
                schedule: DisposalSchedule::after_first_miss(),
            });
        }
    }

    /// Whether a closed family's Owner is still being torn down.
    #[cfg(test)]
    pub(crate) fn disposal_pending(&self, family: AddressFamily) -> bool {
        self.disposing[family.index()].is_some()
    }

    /// Retry every pending family disposal whose retry is due: one bounded
    /// attempt per Owner per call. The attempt drives the runtime, so cancelled
    /// tasks and kernel-owned sends keep progressing instead of waiting for the
    /// shard to be destroyed. The shard's park bound wakes exactly when the
    /// next attempt is due.
    pub(super) fn drive_disposals(&mut self) {
        self.assert_home_thread();
        let now = Instant::now();
        for index in 0..AddressFamily::COUNT {
            if self.disposing[index]
                .as_ref()
                .is_none_or(|disposing| !disposing.schedule.due(now))
            {
                continue;
            }
            let Some(mut disposing) = self.disposing[index].take() else {
                continue;
            };
            let quiescent = self.runtime.block_on(
                disposing
                    .owner
                    .owner
                    .shutdown_and_drain(DISPOSAL_RETRY_BOUND),
            );
            if quiescent {
                drop(disposing.owner);
            } else {
                disposing.schedule.bump(Instant::now());
                self.disposing[index] = Some(disposing);
            }
        }
    }

    /// Tear down a still-disposing Owner during shard shutdown. Nothing is
    /// retried after this: the shard's runtime is destroyed with the shard,
    /// which is what finally releases anything the teardown could not.
    pub(super) fn shutdown_disposals(&mut self, timeout: Duration, all: &mut bool) {
        self.assert_home_thread();
        for index in 0..AddressFamily::COUNT {
            let Some(disposing) = self.disposing[index].take() else {
                continue;
            };
            let fault = self.closed[index]
                .as_ref()
                .map(|closed| closed.fault.clone())
                .unwrap_or_else(|| "unknown fault".to_string());
            let mut owner = disposing.owner;
            let quiescent = self
                .runtime
                .block_on(owner.owner.shutdown_and_drain(timeout));
            if !quiescent {
                *all = false;
                self.shutdown_incomplete = self.shutdown_incomplete.saturating_add(1);
                tracing::warn!(
                    family = index,
                    fault = %fault,
                    in_flight = owner.owner.tx_in_flight(),
                    "srt egress faulted family Owner did not reach quiescence before shard teardown"
                );
            }
            drop(owner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DISPOSAL_RETRY_INTERVAL, DisposalSchedule};
    use std::time::{Duration, Instant};

    /// A teardown that cannot reach quiescence must not retry at a fixed or
    /// unbounded rate: the spacing doubles per miss and stops at the ceiling,
    /// so the shard's sibling family is never held for long while the runtime
    /// still keeps being driven.
    #[test]
    fn disposal_retries_back_off_to_a_bounded_interval() {
        let before = Instant::now();
        let mut schedule = DisposalSchedule::after_first_miss();
        let first = schedule.next_attempt();
        assert!(
            first >= before + DISPOSAL_RETRY_INTERVAL
                && first <= Instant::now() + DISPOSAL_RETRY_INTERVAL,
            "the first retry waits one interval"
        );
        assert!(schedule.due(first), "a retry is due at its own deadline");
        assert!(
            !schedule.due(first.checked_sub(Duration::from_nanos(1)).unwrap_or(before)),
            "and not before it"
        );

        let now = Instant::now();
        for expected_ms in [20u64, 40, 80, 160, 250, 250, 250] {
            schedule.bump(now);
            let spacing = schedule.next_attempt().saturating_duration_since(now);
            assert_eq!(
                spacing,
                Duration::from_millis(expected_ms),
                "spacing doubles to the ceiling and stops there"
            );
        }
    }
}
