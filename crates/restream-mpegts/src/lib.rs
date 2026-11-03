//! Low-level MPEG-TS muxing, demuxing, and metadata extraction shared by
//! ingest, HLS, recording, and transcoding paths.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

use restream_core::VideoMeta;

mod demux;
mod mux;
mod probe;
pub mod wire;

pub use demux::{DemuxProbe, StreamKind, TopologyChanges, TsDemuxer};
pub use mux::{MuxStreamConfig, PacketMeta, TsMuxer, TsServiceMetadata};

pub fn hevc_video_meta(annex_b: &[u8]) -> VideoMeta {
    let mut meta = probe::probe_video(StreamKind::H265, 0, None, None, annex_b);
    meta.pid = None;
    meta
}

pub fn h264_video_meta(annex_b: &[u8]) -> VideoMeta {
    let mut meta = probe::probe_video(StreamKind::H264, 0, None, None, annex_b);
    meta.pid = None;
    meta
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../crates/restream-engine/src/test_fixtures/paths.rs"]
mod test_fixtures;

#[cfg(test)]
mod tests;
