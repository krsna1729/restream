//! Media stack — in-process RTMP/SRT ingest, ring buffer fan-out, FFmpeg muxing/transcoding.
//!
//! No external MediaMTX or spawned FFmpeg child processes. All media flows through
//! `RingBuffer` (lock-free, cache-line aligned) with `MemoryQueue`-backed AVIO for
//! FFmpeg integration. Supports H.264, H.265/HEVC, and multi-track audio.

pub mod avio;
pub mod codec;
pub mod egress;
pub mod engine;
mod engine_egress;
pub mod engine_hls;
mod engine_ingest;
mod engine_ingest_metadata;
mod engine_pipeline;
pub mod engine_registries;
mod engine_runtime;
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
pub mod recording;
pub mod ring_buffer;
pub mod rtmp;
pub mod security;
pub mod snapshots;
pub mod srt;
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

#[cfg(test)]
mod namespace_tests {
    #[test]
    fn recording_namespaces_are_exported() {
        let _ = std::mem::size_of::<crate::media::recording::runtime::RecordingStart>();
        let _ = std::mem::size_of::<crate::media::recording::writer::RecordingStart>();
    }
}
