//! Native RTMP ingest and egress using `rml_rtmp`.
//!
//! TCP admission, ingest sessions, and egress publication are separate owners.
//! The public entry points remain re-exported here for callers.

mod egress;
mod egress_metadata;
mod egress_packets;
mod egress_transport;
mod enhanced;
mod flv;
mod handshake;
mod ingest;
mod ingest_packets;
mod listener;
mod play;
mod timestamps;

pub use egress::start_rtmp_egress;
pub use listener::{start_rtmp_server, start_rtmp_server_on};

#[cfg(test)]
#[path = "rtmp/tests.rs"]
mod tests;

#[cfg(test)]
use bytes::Bytes;
#[cfg(test)]
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
};
#[cfg(test)]
use rml_rtmp::time::RtmpTimestamp;
#[cfg(test)]
use std::{sync::Arc, time::Duration};
#[cfg(test)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[cfg(test)]
use tokio::net::{TcpListener, TcpStream};
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::media::ingest_auth::{PipelineAccessAuthenticator, PipelineAccessMode};
#[cfg(test)]
use crate::media::packet::{MediaPacket, PayloadFormat};
#[cfg(test)]
use crate::media::security::IngestSecurityService;

#[cfg(test)]
use egress_metadata::{
    RTMP_METADATA_VIDEO_CODEC_ID_HEVC, resolved_output_audio_tracks, rtmp_publish_metadata,
    validate_rtmp_output_audio_tracks,
};
#[cfg(test)]
use egress_packets::{
    cache_h264_parameter_sets, h264_sequence_header_for_keyframe,
    resolve_deferred_audio_sequence_header, rtmp_output_waits_for_video,
    rtmp_video_packet_can_be_dropped, rtmp_warmup_ready, should_defer_audio_until_video_ready,
    should_send_startup_audio_sequence_header, startup_video_sequence_header,
    validate_rtmp_output_audio_packet_track,
};
#[cfg(test)]
use egress_transport::parse_rtmp_url;
#[cfg(test)]
use enhanced::{enhanced_rtmp_connect_packet, raw_packet_starts_with_hevc_parameter_set};
#[cfg(test)]
use flv::{
    FlvVideoPacketKind, classify_flv_video_packet, flv_avcc_config_annexb_parameter_sets,
    flv_video_composition_time_ms, parse_flv_audio_meta, parse_flv_video_meta,
};
#[cfg(test)]
use handshake::perform_client_handshake;
#[cfg(test)]
use ingest::handle_rtmp_client;
#[cfg(test)]
use timestamps::{RtmpTimestampGuard, refreshed_video_sequence_header_timestamp};
