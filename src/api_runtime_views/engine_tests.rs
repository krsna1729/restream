//! Runtime-view assertions (health, output status, processing graph) over a
//! live `MediaEngine`.
use crate::media::engine::*;
use crate::media::snapshots::PublisherQuality;
use crate::media::test_support::{test_audio_packet, test_video_packet};
use proptest::prelude::*;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_domain::domain::state::{DesiredOutputState, EgressPhase, EgressPhase as EP};
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
    crate::api_runtime_views::health_snapshot(
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
    crate::api_runtime_views::health_snapshot(
        engine,
        &crate::AppConfig::default(),
        pipeline_ids,
        recording_enabled,
        disconnect_grace_ms,
    )
    .await
}

async fn test_health_summary_snapshot(engine: &MediaEngine) -> serde_json::Value {
    crate::api_runtime_views::health_summary_snapshot(
        engine,
        &crate::AppConfig::default(),
        &[],
        &HashMap::new(),
        0,
    )
    .await
}

#[path = "engine_tests/egress.rs"]
mod egress;
#[path = "engine_tests/egress_fabric.rs"]
mod egress_fabric;
#[path = "engine_tests/graph.rs"]
mod graph;
#[path = "engine_tests/ingest_registry.rs"]
mod ingest_registry;
#[path = "engine_tests/lifecycle_egress.rs"]
mod lifecycle_egress;
#[path = "engine_tests/lifecycle_ingest.rs"]
mod lifecycle_ingest;
#[path = "engine_tests/lifecycle_properties.rs"]
mod lifecycle_properties;
#[path = "engine_tests/lifecycle_ring.rs"]
mod lifecycle_ring;
#[path = "engine_tests/snapshots.rs"]
mod snapshots;
