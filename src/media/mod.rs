//! Media stack — RTMP/SRT ingest, ring-buffer fan-out, muxing, transcoding, HLS, and recording.
//!
//! Transcoding and file ingest run as managed `ffmpeg` child processes
//! ([`external_transcoder`], `external_file_ingest`).
//!
//! Live packets flow through [`ring_buffer`] (lock-free SPMC). MediaMTX is not a
//! runtime dependency; it appears only as an optional live-harness peer.
//! Supports H.264, H.265/HEVC, and multi-track audio.

pub mod audio_router;
#[forbid(unsafe_code)]
pub mod byte_queue;
#[forbid(unsafe_code)]
pub mod codec;
pub mod egress;
pub mod engine;
mod engine_egress;
mod engine_egress_fabric;
pub(crate) mod engine_egress_fabric_diagnostics;
pub mod engine_hls;
pub(crate) mod engine_hls_egress_fabric;
mod engine_ingest;
mod engine_ingest_metadata;
mod engine_moq_egress_fabric;
mod engine_pipeline;
mod engine_pipeline_egress_fabric;
pub mod engine_registries;
mod engine_rtmp_egress_fabric;
mod engine_runtime;
mod engine_sink_egress_fabric;
mod engine_snapshots;
pub(crate) mod executor;
pub(crate) mod external_file_ingest;
pub mod external_transcoder;
pub mod feeder;
pub mod file_analysis;
pub mod hls;
pub mod poster;
pub use hls::fmp4 as hls_fmp4;
pub use hls::upload as hls_upload;
pub mod hls_ingest;
pub mod hls_pull;
pub mod ingest_auth;
pub mod input_gate;
pub mod metadata;
pub(crate) mod moq;
#[doc(hidden)]
pub use moq::bench_entry as moq_bench_entry;
#[cfg(fuzzing)]
#[doc(hidden)]
pub use moq::fuzz_entry as moq_fuzz_entry;
#[doc(hidden)]
pub use moq::subscribe_entry as moq_subscribe_entry;
#[forbid(unsafe_code)]
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
pub mod stage_lease;
pub mod stage_lifecycle;
pub mod stage_metrics;
pub mod stage_registry_access;
pub mod stage_runtime;
pub mod standby_gop;
pub mod startup_policy;
pub mod stream_inspector;
pub mod stream_qa;

pub mod ffmpeg;
pub mod ingest_publisher;
pub mod tcp_stats;
pub mod timing;
pub mod ts_chunk_ring;
pub mod uring_capabilities;

use ring_buffer::MEDIA_PULL_BURST_PACKETS;

/// One fixed-size MPEG-TS packet.
pub const MPEG_TS_PACKET_BYTES: usize = 188;

/// Seven TS packets fit exactly in one 1316-byte SRT payload.
pub const MPEG_TS_PACKETS_PER_SRT_PAYLOAD: usize = 7;

/// SRT payload size that keeps MPEG-TS frames whole.
pub const SRT_TS_PAYLOAD_BYTES: usize = MPEG_TS_PACKET_BYTES * MPEG_TS_PACKETS_PER_SRT_PAYLOAD;

/// Reusable MPEG-TS batch capacity aligned to full 1316-byte SRT payloads.
pub const MEDIA_TS_BATCH_TARGET_BYTES: usize = SRT_TS_PAYLOAD_BYTES * MEDIA_PULL_BURST_PACKETS;
