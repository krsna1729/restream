//! Media stack — in-process RTMP/SRT ingest, ring buffer fan-out, FFmpeg muxing/transcoding.
//!
//! No external MediaMTX or spawned FFmpeg child processes. All media flows through
//! `RingBuffer` (lock-free, cache-line aligned) with `MemoryQueue`-backed AVIO for
//! FFmpeg integration. Supports H.264, H.265/HEVC, and multi-track audio.

pub mod avio;
pub mod codec;
pub mod engine;
pub mod engine_hls;
pub mod engine_registries;
mod engine_snapshots;
pub mod external_transcoder;
pub mod feeder;
pub mod file_analysis;
pub mod file_ingest;
pub mod h264_transcoder;
pub mod hls;
pub mod hls_fmp4;
pub mod hls_preview_runtime;
pub mod hls_upload;
pub mod mpegts;
pub mod pipe_metrics;
pub mod profiles;
pub mod recording;
pub mod ring_buffer;
pub mod rtmp;
pub mod security;
pub mod srt;
pub mod stage_lifecycle;
pub mod stage_metrics;
pub mod stage_registry_access;
pub mod stage_runtime;
pub mod startup_policy;

pub mod ffmpeg;
pub mod tcp_stats;
pub mod timing;
pub mod transcoder;
pub mod ts_chunk_ring;

/// Max media packets a runtime reader processes per hot-loop burst.
pub const MEDIA_PULL_BURST_PACKETS: usize = 32;

/// Soft cap for producer-side ring publications from demux/transcode drains.
pub const MEDIA_PRODUCER_BATCH_PACKETS: usize = MEDIA_PULL_BURST_PACKETS;

/// One fixed-size MPEG-TS packet.
pub const MPEG_TS_PACKET_BYTES: usize = 188;

/// Seven TS packets fit exactly in one 1316-byte SRT payload.
pub const MPEG_TS_PACKETS_PER_SRT_PAYLOAD: usize = 7;

/// SRT payload size that keeps MPEG-TS frames whole.
pub const SRT_TS_PAYLOAD_BYTES: usize = MPEG_TS_PACKET_BYTES * MPEG_TS_PACKETS_PER_SRT_PAYLOAD;

/// Reusable MPEG-TS batch capacity aligned to full 1316-byte SRT payloads.
pub const MEDIA_TS_BATCH_TARGET_BYTES: usize = SRT_TS_PAYLOAD_BYTES * MEDIA_PULL_BURST_PACKETS;
