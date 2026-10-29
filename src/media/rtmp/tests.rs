use super::*;
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{AuthenticatedPipeline, PipelineAccessFuture};
use restream_core::MediaType;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_rtmp::{egress_connection, egress_transport, startup_video_sequence_header};
use restream_sync::RingBuffer;

include!("tests/handshake.rs");
include!("tests/parser_memory.rs");
include!("tests/egress_startup.rs");
include!("tests/flv.rs");
include!("tests/admission.rs");
