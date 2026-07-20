use super::*;
use crate::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use crate::domain::srt_ingest::SrtGlobalIngestConfig;
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::security::IngestSecurityService;
use crate::media::ts_chunk_ring::TsChunkRing;
use proptest::prelude::*;
use tokio_util::sync::CancellationToken;

include!("srt_tests/policy.rs");
include!("srt_tests/quality.rs");
include!("srt_tests/muxing.rs");
include!("srt_tests/socket_runtime.rs");
include!("srt_tests/readiness.rs");

#[path = "srt_tests/shared_muxer.rs"]
mod shared_muxer;
