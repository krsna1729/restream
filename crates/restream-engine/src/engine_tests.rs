use super::*;
use crate::engine::MediaEngine;
use crate::engine_hls::HlsConsumers;
use crate::pipe_metrics::PipeMetrics;
use crate::test_support::{test_audio_packet, test_video_packet};
use bytes::Bytes;
use proptest::prelude::*;
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_domain::domain::stage::StageKind;
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
