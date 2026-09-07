//! Media stack — RTMP/SRT ingest, ring-buffer fan-out, muxing, transcoding, HLS, and recording.
//!
//! Two FFmpeg backends exist:
//!
//! - **External (production default):** managed `ffmpeg` child processes for
//!   transcode ([`external_transcoder`]) and file ingest (`external_file_ingest`).
//! - **In-process:** FFmpeg via `MemoryQueue`-backed AVIO on guarded OS threads
//!   ([`transcoder`], [`h264_transcoder`], optional internal [`file_ingest`]).
//!
//! Live packets flow through [`ring_buffer`] (lock-free SPMC). MediaMTX is not a
//! runtime dependency; it appears only as an optional live-harness peer.
//! Supports H.264, H.265/HEVC, and multi-track audio.

pub mod avio;
pub mod codec;
pub mod egress;
pub mod engine;
mod engine_egress;
mod engine_egress_fabric;
pub(crate) mod engine_egress_fabric_diagnostics;
pub mod engine_hls;
mod engine_ingest;
mod engine_ingest_metadata;
mod engine_pipeline;
mod engine_pipeline_egress_fabric;
pub mod engine_registries;
mod engine_rtmp_egress_fabric;
mod engine_runtime;
mod engine_sink_egress_fabric;
mod engine_snapshots;
pub(crate) mod external_file_ingest;
pub mod external_transcoder;
pub mod feeder;
pub mod file_analysis;
pub mod file_ingest;
pub mod h264_transcoder;
pub mod hls;
pub use hls::fmp4 as hls_fmp4;
pub use hls::preview as hls_preview_runtime;
pub use hls::upload as hls_upload;
pub mod ingest_auth;
pub mod input_gate;
pub mod metadata;
pub mod mpegts;
pub mod packet;
pub mod pipe_metrics;
pub mod profiles;
pub mod recirculation;
pub mod recording;
pub mod ring_buffer;
pub mod rtmp;
pub mod security;
pub mod snapshots;
pub mod srt;
pub(crate) mod srt_stream_id;
pub mod stage_lifecycle;
pub mod stage_metrics;
pub mod stage_registry_access;
pub mod stage_runtime;
pub mod standby_gop;
pub mod startup_policy;

pub mod ffmpeg;
pub mod tcp_stats;
pub mod timing;
pub mod transcoder;
pub mod ts_chunk_ring;

use ring_buffer::MEDIA_PULL_BURST_PACKETS;

/// One fixed-size MPEG-TS packet.
pub const MPEG_TS_PACKET_BYTES: usize = 188;

/// Seven TS packets fit exactly in one 1316-byte SRT payload.
pub const MPEG_TS_PACKETS_PER_SRT_PAYLOAD: usize = 7;

/// SRT payload size that keeps MPEG-TS frames whole.
pub const SRT_TS_PAYLOAD_BYTES: usize = MPEG_TS_PACKET_BYTES * MPEG_TS_PACKETS_PER_SRT_PAYLOAD;

/// Reusable MPEG-TS batch capacity aligned to full 1316-byte SRT payloads.
pub const MEDIA_TS_BATCH_TARGET_BYTES: usize = SRT_TS_PAYLOAD_BYTES * MEDIA_PULL_BURST_PACKETS;
