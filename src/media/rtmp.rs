//! Native RTMP ingest and egress using `rml_rtmp`.
//!
//! TCP admission, ingest sessions, and egress publication are separate owners.
//! The public entry points remain re-exported here for callers.

mod egress;
mod egress_connection;
mod egress_engine;
mod egress_metadata;
mod egress_packets;
mod egress_transport;
pub(crate) mod egress_write;
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

pub(crate) use egress_connection::{RtmpSessionCore, RtmpSessionError, RtmpSessionEvent};
pub(crate) use egress_engine::{RtmpMediaAction, RtmpMediaEncoder};
pub(crate) use egress_metadata::{
    output_ring_video_codec_kind, resolved_output_audio_tracks, rtmp_publish_metadata,
    validate_rtmp_output_audio_tracks,
};
pub(crate) use egress_packets::{
    h264_sps_nalu, resolve_deferred_audio_sequence_header, should_defer_audio_until_video_ready,
    should_send_startup_audio_sequence_header, startup_video_sequence_header,
    validate_rtmp_output_audio_packet_track,
};
pub(crate) use egress_transport::{
    RtmpUrlParts, parse_rtmp_url, resolve_rtmps_client_config, rustls_client_config,
};

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
use egress_metadata::RTMP_METADATA_VIDEO_CODEC_ID_HEVC;
#[cfg(test)]
use egress_packets::{
    cache_h264_parameter_sets, h264_sequence_header_for_keyframe, rtmp_output_waits_for_video,
    rtmp_video_packet_can_be_dropped, rtmp_warmup_ready,
};
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
