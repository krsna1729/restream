//! Runtime-view assertions (health, output status, processing graph) over a
//! live `MediaEngine`.
use proptest::prelude::*;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_domain::domain::state::{
    DesiredOutputState, EGRESS_PROGRESS_STALE_MS, EgressPhase, EgressPhase as EP,
};
use restream_engine::engine::*;
use restream_engine::snapshots::PublisherQuality;
use restream_engine::test_support::{test_audio_packet, test_video_packet};
use restream_sync::Reader;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

async fn test_health_snapshot(
    engine: &MediaEngine,
    pipeline_ids: &[String],
    recording_enabled: &HashMap<String, bool>,
) -> serde_json::Value {
    crate::api::runtime_views::health_snapshot(
        engine,
        &crate::AppConfig::default(),
        pipeline_ids,
        recording_enabled,
        0,
    )
    .await
}

async fn test_health_snapshot_with_disconnect_grace(
    engine: &MediaEngine,
    pipeline_ids: &[String],
    recording_enabled: &HashMap<String, bool>,
    disconnect_grace_ms: u64,
) -> serde_json::Value {
    crate::api::runtime_views::health_snapshot(
        engine,
        &crate::AppConfig::default(),
        pipeline_ids,
        recording_enabled,
        disconnect_grace_ms,
    )
    .await
}

async fn test_health_summary_snapshot(engine: &MediaEngine) -> serde_json::Value {
    crate::api::runtime_views::health_summary_snapshot(
        engine,
        &crate::AppConfig::default(),
        &[],
        &HashMap::new(),
        0,
    )
    .await
}

mod egress;
mod egress_fabric;
mod graph;
mod ingest_registry;
mod lifecycle_egress;
mod lifecycle_ingest;
mod lifecycle_properties;
mod lifecycle_ring;
mod snapshots;
