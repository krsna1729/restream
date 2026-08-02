use super::*;
use crate::domain::stage::StageKind;
use crate::domain::state::{DesiredOutputState, EgressPhase as EP};
use crate::media::engine::MediaEngine;
use crate::media::engine_hls::HlsConsumers;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::ring_buffer::Reader;
use bytes::Bytes;
use proptest::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;

#[path = "engine_lifecycle_tests.rs"]
mod engine_lifecycle_tests;
#[path = "engine_poison_recovery_tests.rs"]
mod engine_poison_recovery_tests;
#[path = "engine_stage_tests.rs"]
mod engine_stage_tests;

async fn test_health_snapshot(
    engine: &MediaEngine,
    pipeline_ids: &[String],
    recording_enabled: &HashMap<String, bool>,
) -> serde_json::Value {
    crate::api_runtime_views::health_snapshot(engine, pipeline_ids, recording_enabled, 0).await
}

async fn test_health_snapshot_with_disconnect_grace(
    engine: &MediaEngine,
    pipeline_ids: &[String],
    recording_enabled: &HashMap<String, bool>,
    disconnect_grace_ms: u64,
) -> serde_json::Value {
    crate::api_runtime_views::health_snapshot(
        engine,
        pipeline_ids,
        recording_enabled,
        disconnect_grace_ms,
    )
    .await
}

async fn test_health_summary_snapshot(engine: &MediaEngine) -> serde_json::Value {
    crate::api_runtime_views::health_summary_snapshot(engine, &[], &HashMap::new(), 0).await
}

#[path = "engine_tests/dependencies.rs"]
mod dependency_tests;
#[path = "engine_tests/egress_fabric.rs"]
mod egress_fabric_tests;
#[path = "engine_tests/egress.rs"]
mod egress_tests;
#[path = "engine_tests/graph.rs"]
mod graph_tests;
#[path = "engine_tests/ingest_registry.rs"]
mod ingest_registry_tests;
#[path = "engine_tests/snapshots.rs"]
mod snapshot_tests;

fn test_video_packet(pts: i64, dts: i64, keyframe: bool) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: keyframe,
        track_index: 0,
        pts,
        dts,
        payload: Bytes::from_static(b"video"),
    }
}

fn test_audio_packet(pts: i64, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts,
        dts,
        payload: Bytes::from_static(b"audio"),
    }
}
