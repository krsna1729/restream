use super::*;
use crate::domain::stage::StageKind;
use crate::media::engine::MediaEngine;
use crate::media::engine_hls::HlsConsumers;
use crate::media::pipe_metrics::PipeMetrics;
use bytes::Bytes;
use proptest::prelude::*;
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_sync::Reader;
use std::sync::Arc;
use tokio::process::Command;

#[path = "engine_ingest_tests.rs"]
mod engine_ingest_tests;
#[path = "engine_lifecycle_tests.rs"]
mod engine_lifecycle_tests;
#[path = "engine_poison_recovery_tests.rs"]
mod engine_poison_recovery_tests;
#[path = "engine_stage_tests.rs"]
mod engine_stage_tests;

#[path = "engine_tests/dependencies.rs"]
mod dependency_tests;
#[path = "engine_tests/egress_fabric.rs"]
pub(crate) mod egress_fabric_tests;
#[path = "engine_tests/egress.rs"]
mod egress_tests;
#[path = "engine_tests/graph.rs"]
mod graph_tests;
#[path = "engine_tests/ingest_registry.rs"]
mod ingest_registry_tests;
#[path = "engine_tests/snapshots.rs"]
mod snapshot_tests;

pub(crate) fn test_video_packet(pts: i64, dts: i64, keyframe: bool) -> MediaPacket {
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

pub(crate) fn test_audio_packet(pts: i64, dts: i64) -> MediaPacket {
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
