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
        })
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
