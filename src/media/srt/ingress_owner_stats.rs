//! The ingress Owner's share of the listener-wide `ingressOwner` stats block.
//! Split from `ingress_owner` for size; it is part of the same owner-thread
//! state.

use std::sync::atomic::{AtomicU64, Ordering};

use srt_transport::compio::OwnerServiceReport;

use super::OwnerLoop;

/// One Owner's share of the gauges and Owner-cumulative counters in the
/// listener-wide [`ListenerSocketStats`](crate::media::snapshots::ListenerSocketStats):
/// each Owner adds the change since its last report, so the block reports the
/// sum over every Owner, and an exiting Owner withdraws its gauges.
#[derive(Default)]
pub(super) struct Published {
    pub(super) tx_capacity: u64,
    tx_in_flight: u64,
    tx_exhaustions: u64,
    rx_ring_depth: u64,
    rx_ring_dropped: u64,
    rx_buffer_exhaustions: u64,
    rx_truncated: u64,
    policy_requests: u64,
    policy_rejections: u64,
    policy_deferred: u64,
    credential_failures: u64,
    peers: u64,
    pub(super) deferred_sends: u64,
}

/// Move `target` by this Owner's change from `last` to `value`. An unchanged
/// value touches nothing: the block is shared by every Owner of the listener.
pub(super) fn publish(target: &AtomicU64, last: &mut u64, value: u64) {
    if value > *last {
        target.fetch_add(value - *last, Ordering::Relaxed);
    } else if value < *last {
        target.fetch_sub(*last - value, Ordering::Relaxed);
    }
    *last = value;
}

/// Add `value` to a shared counter unless it is zero.
fn count(target: &AtomicU64, value: u64) {
    if value != 0 {
        target.fetch_add(value, Ordering::Relaxed);
    }
}

impl OwnerLoop {
    pub(super) fn account_service(&mut self, report: &OwnerServiceReport) {
        let stats = &self.stats.ingress_owner;
        stats.service_visits.fetch_add(1, Ordering::Relaxed);
        count(&stats.service_actions, report.actions as u64);
        count(
            &stats.maintenance_actions,
            report.maintenance_actions as u64,
        );
        count(&stats.budget_exhausted, u64::from(report.budget_exhausted));
        count(&stats.rx_packets, report.rx_packets as u64);
        count(&stats.rx_bytes, report.rx_bytes as u64);
        count(&stats.tx_packets, report.tx_packets_submitted as u64);
        count(&stats.tx_completed_ok, report.tx_completed_ok as u64);
        count(
            &stats.tx_failed,
            (report.tx_failed_sends + report.tx_short_sends) as u64,
        );
        let tx = self.owner.tx_pool_snapshot();
        let published = &mut self.published;
        publish(
            &stats.tx_in_flight,
            &mut published.tx_in_flight,
            self.owner.tx_in_flight() as u64,
        );
        stats
            .tx_high_water
            .fetch_max(tx.high_water as u64, Ordering::Relaxed);
        publish(
            &stats.tx_exhaustions,
            &mut published.tx_exhaustions,
            tx.exhaustions,
        );
        if let Some(rx) = self.owner.rx_stats().listener {
            publish(
                &stats.rx_ring_depth,
                &mut published.rx_ring_depth,
                rx.depth as u64,
            );
            publish(
                &stats.rx_ring_dropped,
                &mut published.rx_ring_dropped,
                rx.dropped,
            );
            publish(
                &stats.rx_buffer_exhaustions,
                &mut published.rx_buffer_exhaustions,
                rx.buffer_exhaustions,
            );
            publish(
                &stats.rx_truncated,
                &mut published.rx_truncated,
                rx.truncated,
            );
        }
        if let Some(telemetry) = self.owner.listener_telemetry() {
            publish(
                &stats.policy_requests,
                &mut published.policy_requests,
                telemetry.policy_requests,
            );
            publish(
                &stats.policy_rejections,
                &mut published.policy_rejections,
                telemetry.policy_rejections,
            );
            publish(
                &stats.policy_deferred,
                &mut published.policy_deferred,
                telemetry.policy_deferred,
            );
            publish(
                &stats.credential_failures,
                &mut published.credential_failures,
                telemetry.credential_failures,
            );
        }
        publish(&stats.peers, &mut published.peers, self.peers);
    }

    /// Withdraw this Owner's gauges from the listener-wide block when it
    /// exits; its cumulative counters stay counted.
    pub(super) fn withdraw_gauges(&mut self) {
        let (stats, published) = (&self.stats.ingress_owner, &mut self.published);
        publish(&stats.tx_capacity, &mut published.tx_capacity, 0);
        publish(&stats.tx_in_flight, &mut published.tx_in_flight, 0);
        publish(&stats.rx_ring_depth, &mut published.rx_ring_depth, 0);
        publish(&stats.peers, &mut published.peers, 0);
        publish(&stats.deferred_sends, &mut published.deferred_sends, 0);
    }
}
