//! RTMP ingest and egress protocol handling using `rml_rtmp`.
//!
//! TCP admission, ingest sessions, and egress publication are separate owners.
//! The public entry points remain re-exported here for callers.

mod egress_metadata;
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz_entry;
// Publisher C0/C1/C2 bytes, before admission (isolation audit F6).
#[deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
mod handshake;
// RTMP ingest reads every publisher byte: no unchecked indexing, arithmetic
// or unwrapping (isolation audit F6). The attribute sits here to keep
// ingest.rs under the line cap; it covers ingest/session.rs as well.
#[deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
mod ingest;
#[forbid(unsafe_code)]
mod ingest_media;
mod listener;
// Client play commands (isolation audit F6).
#[deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
mod play;

pub(crate) use listener::start_rtmp_server_on_with_shutdown;
pub use listener::{start_rtmp_server, start_rtmp_server_on};

pub(crate) use egress_metadata::{
    output_ring_video_codec_kind, resolved_output_audio_tracks, rtmp_publish_metadata,
};

#[cfg(test)]
#[path = "rtmp/tests.rs"]
mod tests;

#[cfg(test)]
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
};
#[cfg(test)]
use std::{sync::Arc, time::Duration};
#[cfg(test)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(test)]
use tokio::net::TcpStream;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::media::ingest_auth::{PipelineAccessAuthenticator, PipelineAccessMode};
#[cfg(test)]
use crate::media::security::IngestSecurityService;

#[cfg(test)]
use handshake::perform_client_handshake;
#[cfg(test)]
use restream_rtmp::flv::flv_config_annexb_parameter_sets;
#[cfg(test)]
use restream_rtmp::{
    FlvVideoPacketKind, RTMP_METADATA_VIDEO_CODEC_ID_HEVC, classify_flv_video_packet,
    flv_video_composition_time_ms, parse_flv_video_meta,
};
