//! Disposing a family Owner: the record that keeps the family closed and
//! visible, plus the teardown that releases what a faulted Owner can no longer
//! reclaim. A child of `owner_set`, so it reaches the shard's Owner slots and
//! counters directly instead of adding accessors.

use super::{AddressFamily, FamilyOwner, OWNER_SHUTDOWN_RESERVE, SrtOwners};
use crate::media::egress::metrics::OwnerFamilyMetrics;
use srt_transport::compio::OwnerRxMode;

/// A family Owner that faulted and was disposed: the fault text keeps
/// admission refused after the Owner itself is gone, and the last metrics row
/// keeps the fault and the final counters visible on the shard's metrics.
pub(super) struct DisposedFamily {
    pub(super) fault: String,
    pub(super) metrics: OwnerFamilyMetrics,
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
    /// the life of the shard. Tear it down under the reserve the shard keeps
    /// for Owner teardown, then drop it: the drop is what releases the
    /// retained protocol storage. The family stays closed for admission and
    /// keeps its last metrics row.
    pub(crate) fn dispose_faulted(&mut self, family: AddressFamily) {
        self.assert_home_thread();
        let index = family.index();
        let Some(mut family_owner) = self.owners[index].take() else {
            return;
        };
        let fault = family_owner
            .owner
            .fault()
            .map(|fault| format!("{fault:?}"))
            .unwrap_or_else(|| "unknown fault".to_string());
        let mut metrics = family_owner.metrics();
        metrics.faulted = true;
        let quiescent = self.runtime.block_on(
            family_owner
                .owner
                .shutdown_and_drain(OWNER_SHUTDOWN_RESERVE),
        );
        if !quiescent {
            self.shutdown_incomplete = self.shutdown_incomplete.saturating_add(1);
            tracing::warn!(
                ?family,
                fault = %fault,
                in_flight = family_owner.owner.tx_in_flight(),
                "srt egress faulted Owner did not reach quiescence within its bound; disposed anyway"
            );
        }
        self.disposed[index] = Some(DisposedFamily { fault, metrics });
    }
}
