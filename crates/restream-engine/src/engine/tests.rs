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

mod ingest;
mod lifecycle;
mod poison_recovery;
mod stage;

mod dependencies;
mod egress;
pub(crate) mod egress_fabric;
mod graph;
mod ingest_registry;
mod snapshots;
