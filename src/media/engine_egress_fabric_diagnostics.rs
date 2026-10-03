//! Cross-protocol fabric shard diagnostics — combines the four per-protocol
//! registries (SRT, RTMP, sink, pipeline) into one operator-facing view.
//! This is the production (non-test) counterpart to the per-registry
//! `#[cfg(test)]`-only snapshot accessors: it powers the resource map's
//! shard-thread accounting and health-derived alerts, not just assertions.

use serde::Serialize;
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
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TxClassJson {
    data_first: u64,
    data_retransmit: u64,
    ack: u64,
    ackack: u64,
    nak: u64,
    keepalive: u64,
    handshake: u64,
    drop_request: u64,
    key_material: u64,
    shutdown: u64,
    other_control: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SrtOwnerDiagnosticsJson<'a> {
    family: &'static str,
    present: bool,
    faulted: bool,
    managed_rx: bool,
    tx_capacity: u32,
    tx_free: u32,
    tx_high_water: u32,
    tx_exhaustions: u64,
    tx_in_flight: u32,
    tx_packets: u64,
    tx_bytes: u64,
    tx_class: TxClassJson,
    tx_batched_sends: u64,
    tx_coalesced_datagrams: u64,
    tx_gso_fallbacks: u64,
    tx_completed_ok: u64,
    tx_short_sends: u64,
    tx_failed_sends: u64,
    tx_peer_local_failures: u64,
    tx_transient_failures: u64,
    protocol_output_failures: u64,
    rx_packets: u64,
    rx_bytes: u64,
    rx_ring_depth: u32,
    rx_ring_dropped: u64,
    rx_buffer_exhaustions: u64,
    rx_truncated: u64,
    service_visits: u64,
    service_duration_sum_us: u64,
    service_duration_max_us: u64,
    service_actions: u64,
    maintenance_actions: u64,
    service_budget_exhausted: u64,
    caller_in_flight_hwm: u32,
    caller_queued_hwm: u32,
    caller_queue_longest_us: u64,
    caller_in_flight: u32,
    caller_queued: u32,
    caller_expired: u64,
    caller_failed: u64,
    caller_cancelled: u64,
    peer_group_collisions: u64,
    #[serde(skip)]
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> SrtOwnerDiagnosticsJson<'a> {
    fn new(
        family: &'static str,
        owner: &'a crate::media::egress::metrics::OwnerFamilyMetrics,
    ) -> Self {
        Self {
            family,
            present: owner.present,
            faulted: owner.faulted,
            managed_rx: owner.managed_rx,
            tx_capacity: owner.tx_capacity,
            tx_free: owner.tx_free,
            tx_high_water: owner.tx_high_water,
            tx_exhaustions: owner.tx_exhaustions,
            tx_in_flight: owner.tx_in_flight,
            tx_packets: owner.tx_packets,
            tx_bytes: owner.tx_bytes,
            tx_class: TxClassJson {
                data_first: owner.tx_class.data_first,
                data_retransmit: owner.tx_class.data_retx,
                ack: owner.tx_class.ack,
                ackack: owner.tx_class.ackack,
                nak: owner.tx_class.nak,
                keepalive: owner.tx_class.keepalive,
                handshake: owner.tx_class.handshake,
                drop_request: owner.tx_class.dropreq,
                key_material: owner.tx_class.km,
                shutdown: owner.tx_class.shutdown,
                other_control: owner.tx_class.other_control,
            },
            tx_batched_sends: owner.tx_batching.batched_sends,
            tx_coalesced_datagrams: owner.tx_batching.coalesced_datagrams,
            tx_gso_fallbacks: owner.tx_batching.gso_fallbacks,
            tx_completed_ok: owner.tx_completed_ok,
            tx_short_sends: owner.tx_short_sends,
            tx_failed_sends: owner.tx_failed_sends,
            tx_peer_local_failures: owner.tx_peer_local_failures,
            tx_transient_failures: owner.tx_transient_failures,
            protocol_output_failures: owner.protocol_output_failures,
            rx_packets: owner.rx_packets,
            rx_bytes: owner.rx_bytes,
            rx_ring_depth: owner.rx_ring_depth,
            rx_ring_dropped: owner.rx_ring_dropped,
            rx_buffer_exhaustions: owner.rx_buffer_exhaustions,
            rx_truncated: owner.rx_truncated,
            service_visits: owner.service_visits,
            service_duration_sum_us: owner.service_duration_sum_us,
            service_duration_max_us: owner.service_duration_max_us,
            service_actions: owner.service_actions,
            maintenance_actions: owner.maintenance_actions,
            service_budget_exhausted: owner.service_budget_exhausted,
            caller_in_flight_hwm: owner.caller_in_flight_hwm,
            caller_queued_hwm: owner.caller_queued_hwm,
            caller_queue_longest_us: owner.caller_queue_longest_us,
            caller_in_flight: owner.caller_in_flight,
            caller_queued: owner.caller_queued,
            caller_expired: owner.caller_expired,
            caller_failed: owner.caller_failed,
            caller_cancelled: owner.caller_cancelled,
            peer_group_collisions: owner.peer_group_collisions,
            _marker: std::marker::PhantomData,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EgressFabricShardStatusJson<'a> {
    protocol: &'static str,
    feed_id: &'a str,
    shard_index: u32,
    state: &'static str,
    loop_iterations: u64,
    media_ticks: u64,
    progress_age_ms: Option<u64>,
    command_depth: u32,
    command_capacity: u32,
    resync_count: u64,
    ready_depth: u32,
    ready_depth_hwm: u32,
    ready_visits: u64,
    budget_exhaustions: u64,
    rx_packets: u64,
    rx_bytes: u64,
    tx_packets: u64,
    tx_bytes: u64,
    cqes: u64,
    sqes: u64,
    stale_completions: u64,
    rx_pool_empty: u64,
    tx_pool_empty: u64,
    send_zc_attempts: u64,
    send_zc_fallbacks: u64,
    cq_overflows: u64,
    ready_overflows: u64,
    queue_overflows: u64,
    feed_wakes_useful: u64,
    feed_wakes_empty: u64,
    loop_duration_sum_us: u64,
    driver_budget_violations: u64,
    driver_overrun_us: u64,
    retry_events: u64,
    srt_runtime_io_uring: bool,
    srt_managed_rx_available: bool,
    srt_owners: [SrtOwnerDiagnosticsJson<'a>; 2],
}

impl serde::Serialize for EgressFabricShardStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        EgressFabricShardStatusJson {
            protocol: self.protocol,
            feed_id: &self.feed_id,
            shard_index: self.shard_index,
            state: self.state_str(),
            loop_iterations: self.loop_iterations,
            media_ticks: self.media_ticks,
            progress_age_ms: self.progress_age_ms,
            command_depth: self.command_depth,
            command_capacity: self.command_capacity,
            resync_count: self.resync_count,
            ready_depth: self.ready_depth,
            ready_depth_hwm: self.ready_depth_hwm,
            ready_visits: self.ready_visits,
            budget_exhaustions: self.budget_exhaustions,
            rx_packets: self.rx_packets,
            rx_bytes: self.rx_bytes,
            tx_packets: self.tx_packets,
            tx_bytes: self.tx_bytes,
            cqes: self.cqes,
            sqes: self.sqes,
            stale_completions: self.stale_completions,
            rx_pool_empty: self.rx_pool_empty,
            tx_pool_empty: self.tx_pool_empty,
            send_zc_attempts: self.send_zc_attempts,
            send_zc_fallbacks: self.send_zc_fallbacks,
            cq_overflows: self.cq_overflows,
            ready_overflows: self.ready_overflows,
            queue_overflows: self.queue_overflows,
            feed_wakes_useful: self.feed_wakes_useful,
            feed_wakes_empty: self.feed_wakes_empty,
            loop_duration_sum_us: self.loop_duration_sum_us,
            driver_budget_violations: self.driver_budget_violations,
            driver_overrun_us: self.driver_overrun_us,
            retry_events: self.retry_events,
            srt_runtime_io_uring: self.srt_runtime_io_uring,
            srt_managed_rx_available: self.srt_managed_rx_available,
            srt_owners: [
                SrtOwnerDiagnosticsJson::new("v4", &self.srt_owners[0]),
                SrtOwnerDiagnosticsJson::new("v6", &self.srt_owners[1]),
            ],
        }
        .serialize(serializer)
    }
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
