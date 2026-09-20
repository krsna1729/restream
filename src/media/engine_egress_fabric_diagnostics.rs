//! Cross-protocol fabric shard diagnostics — combines the four per-protocol
//! registries (SRT, RTMP, sink, pipeline) into one operator-facing view.
//! This is the production (non-test) counterpart to the per-registry
//! `#[cfg(test)]`-only snapshot accessors: it powers the resource map's
//! shard-thread accounting and health-derived alerts, not just assertions.

use std::time::Duration;

use crate::media::egress::command::FeedId;
use crate::media::egress::shard::{EgressShardHealth, EgressShardHeartbeat};
use crate::media::engine::MediaEngine;

/// A shard's health, tagged with which protocol's fabric registry it
/// belongs to and which prepared feed it's serving.
#[derive(Debug, Clone)]
pub(crate) struct EgressFabricShardStatus {
    pub protocol: &'static str,
    pub feed_id: String,
    pub shard_index: u32,
    pub state: EgressShardHealth,
    pub loop_iterations: u64,
    pub media_ticks: u64,
    pub progress_age_ms: Option<u64>,
    pub command_depth: u32,
    pub command_capacity: u32,
    pub resync_count: u64,
    pub ready_depth: u32,
    pub ready_depth_hwm: u32,
    pub ready_visits: u64,
    pub budget_exhaustions: u64,
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub cqes: u64,
    pub sqes: u64,
    pub stale_completions: u64,
    pub rx_pool_empty: u64,
    pub tx_pool_empty: u64,
    pub send_zc_attempts: u64,
    pub send_zc_fallbacks: u64,
    pub cq_overflows: u64,
    pub ready_overflows: u64,
    pub queue_overflows: u64,
    pub feed_wakes_useful: u64,
    pub feed_wakes_empty: u64,
    pub loop_duration_sum_us: u64,
    pub driver_budget_violations: u64,
    pub driver_overrun_us: u64,
    pub retry_events: u64,
    pub srt_owners: [crate::media::egress::metrics::OwnerFamilyMetrics; 2],
    pub srt_runtime_io_uring: bool,
    pub srt_managed_rx_available: bool,
}

impl EgressFabricShardStatus {
    fn from_heartbeat(
        protocol: &'static str,
        feed_id: FeedId,
        heartbeat: EgressShardHeartbeat,
    ) -> Self {
        Self {
            protocol,
            feed_id: feed_id.as_str().to_string(),
            shard_index: heartbeat.shard_id.index(),
            state: heartbeat.state,
            loop_iterations: heartbeat.loop_iterations,
            media_ticks: heartbeat.media_ticks,
            progress_age_ms: heartbeat.progress_age.map(|age| age.as_millis() as u64),
            command_depth: heartbeat.command_depth,
            command_capacity: heartbeat.command_capacity,
            resync_count: heartbeat.resync_count,
            ready_depth: heartbeat.ready_depth,
            ready_depth_hwm: heartbeat.ready_depth_hwm,
            ready_visits: heartbeat.ready_visits,
            budget_exhaustions: heartbeat.budget_exhaustions,
            rx_packets: heartbeat.rx_packets,
            rx_bytes: heartbeat.rx_bytes,
            tx_packets: heartbeat.tx_packets,
            tx_bytes: heartbeat.tx_bytes,
            cqes: heartbeat.cqes,
            sqes: heartbeat.sqes,
            stale_completions: heartbeat.stale_completions,
            rx_pool_empty: heartbeat.rx_pool_empty,
            tx_pool_empty: heartbeat.tx_pool_empty,
            send_zc_attempts: heartbeat.send_zc_attempts,
            send_zc_fallbacks: heartbeat.send_zc_fallbacks,
            cq_overflows: heartbeat.cq_overflows,
            ready_overflows: heartbeat.ready_overflows,
            queue_overflows: heartbeat.queue_overflows,
            feed_wakes_useful: heartbeat.feed_wakes_useful,
            feed_wakes_empty: heartbeat.feed_wakes_empty,
            loop_duration_sum_us: heartbeat.loop_duration_sum_us,
            driver_budget_violations: heartbeat.driver_budget_violations,
            driver_overrun_us: heartbeat.driver_overrun_us,
            retry_events: heartbeat.retry_events,
            srt_owners: heartbeat.srt_owners,
            srt_runtime_io_uring: heartbeat.srt_runtime_io_uring,
            srt_managed_rx_available: heartbeat.srt_managed_rx_available,
        }
    }

    pub fn state_str(&self) -> &'static str {
        match self.state {
            EgressShardHealth::Healthy => "healthy",
            EgressShardHealth::Stalled => "stalled",
            EgressShardHealth::Stopped => "stopped",
            EgressShardHealth::Panicked => "panicked",
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "protocol": self.protocol,
            "feedId": self.feed_id,
            "shardIndex": self.shard_index,
            "state": self.state_str(),
            "loopIterations": self.loop_iterations,
            "mediaTicks": self.media_ticks,
            "progressAgeMs": self.progress_age_ms,
            "commandDepth": self.command_depth,
            "commandCapacity": self.command_capacity,
            "resyncCount": self.resync_count,
            "readyDepth": self.ready_depth,
            "readyDepthHwm": self.ready_depth_hwm,
            "readyVisits": self.ready_visits,
            "budgetExhaustions": self.budget_exhaustions,
            "rxPackets": self.rx_packets,
            "rxBytes": self.rx_bytes,
            "txPackets": self.tx_packets,
            "txBytes": self.tx_bytes,
            "cqes": self.cqes,
            "sqes": self.sqes,
            "staleCompletions": self.stale_completions,
            "rxPoolEmpty": self.rx_pool_empty,
            "txPoolEmpty": self.tx_pool_empty,
            "sendZcAttempts": self.send_zc_attempts,
            "sendZcFallbacks": self.send_zc_fallbacks,
            "cqOverflows": self.cq_overflows,
            "readyOverflows": self.ready_overflows,
            "queueOverflows": self.queue_overflows,
            "feedWakesUseful": self.feed_wakes_useful,
            "feedWakesEmpty": self.feed_wakes_empty,
            "loopDurationSumUs": self.loop_duration_sum_us,
            "driverBudgetViolations": self.driver_budget_violations,
            "driverOverrunUs": self.driver_overrun_us,
            "retryEvents": self.retry_events,
            "srtRuntimeIoUring": self.srt_runtime_io_uring,
            "srtManagedRxAvailable": self.srt_managed_rx_available,
            "srtOwners": [
                srt_owner_json("v4", &self.srt_owners[0]),
                srt_owner_json("v6", &self.srt_owners[1]),
            ],
        })
    }
}

/// One SRT Compio Owner's low-cardinality gauges/counters (no caller ids).
fn srt_owner_json(
    family: &'static str,
    owner: &crate::media::egress::metrics::OwnerFamilyMetrics,
) -> serde_json::Value {
    serde_json::json!({
        "family": family,
        "present": owner.present,
        "faulted": owner.faulted,
        "managedRx": owner.managed_rx,
        "txCapacity": owner.tx_capacity,
        "txFree": owner.tx_free,
        "txHighWater": owner.tx_high_water,
        "txExhaustions": owner.tx_exhaustions,
        "txInFlight": owner.tx_in_flight,
        "txPackets": owner.tx_packets,
        "txBytes": owner.tx_bytes,
        "txCompletedOk": owner.tx_completed_ok,
        "txShortSends": owner.tx_short_sends,
        "txFailedSends": owner.tx_failed_sends,
        "txPeerLocalFailures": owner.tx_peer_local_failures,
        "txTransientFailures": owner.tx_transient_failures,
        "protocolOutputFailures": owner.protocol_output_failures,
        "rxPackets": owner.rx_packets,
        "rxBytes": owner.rx_bytes,
        "rxRingDepth": owner.rx_ring_depth,
        "rxRingDropped": owner.rx_ring_dropped,
        "rxTruncated": owner.rx_truncated,
        "serviceVisits": owner.service_visits,
        "serviceDurationSumUs": owner.service_duration_sum_us,
        "serviceDurationMaxUs": owner.service_duration_max_us,
        "serviceActions": owner.service_actions,
        "maintenanceActions": owner.maintenance_actions,
        "serviceBudgetExhausted": owner.service_budget_exhausted,
        "callerInFlight": owner.caller_in_flight,
        "callerQueued": owner.caller_queued,
        "callerExpired": owner.caller_expired,
        "callerFailed": owner.caller_failed,
        "callerCancelled": owner.caller_cancelled,
        "peerGroupCollisions": owner.peer_group_collisions,
    })
}

impl MediaEngine {
    /// Every live fabric shard's health, across all four protocol
    /// registries. `stall_after` should track how often the caller polls —
    /// a shard genuinely idle between polls (nothing to send) is not the
    /// same as a stalled one, so this must not be a fixed short constant.
    pub(crate) async fn egress_fabric_shard_statuses(
        &self,
        stall_after: Duration,
    ) -> Vec<EgressFabricShardStatus> {
        let mut statuses = Vec::new();
        for (feed_id, heartbeats) in self.srt_fabric_shard_heartbeats(stall_after).await {
            statuses.extend(
                heartbeats
                    .into_iter()
                    .map(|hb| EgressFabricShardStatus::from_heartbeat("srt", feed_id.clone(), hb)),
            );
        }
        for (feed_id, heartbeats) in self.rtmp_fabric_shard_heartbeats(stall_after).await {
            statuses.extend(
                heartbeats
                    .into_iter()
                    .map(|hb| EgressFabricShardStatus::from_heartbeat("rtmp", feed_id.clone(), hb)),
            );
        }
        for (feed_id, heartbeats) in self.sink_fabric_shard_heartbeats(stall_after).await {
            statuses.extend(
                heartbeats
                    .into_iter()
                    .map(|hb| EgressFabricShardStatus::from_heartbeat("sink", feed_id.clone(), hb)),
            );
        }
        for (feed_id, heartbeats) in self.pipeline_fabric_shard_heartbeats(stall_after).await {
            statuses.extend(heartbeats.into_iter().map(|hb| {
                EgressFabricShardStatus::from_heartbeat("pipeline", feed_id.clone(), hb)
            }));
        }
        statuses
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn egress_fabric_shard_statuses_is_empty_with_no_live_fabric_runtimes() {
        let engine = MediaEngine::new();
        let statuses = engine
            .egress_fabric_shard_statuses(Duration::from_secs(5))
            .await;
        assert!(statuses.is_empty());
    }
}
