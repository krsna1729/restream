//! Low-level MPEG-TS muxing, demuxing, and metadata extraction shared by
//! ingest, HLS, recording, and transcoding paths.

pub use restream_mpegts::{
    DemuxProbe, MuxStreamConfig, PacketMeta, StreamKind, TopologyChanges, TsDemuxer, TsMuxer,
    TsSegmentView, TsServiceMetadata, h264_video_meta, hevc_video_meta, remux_segment_view,
};

#[cfg(test)]
pub(crate) fn test_crc32_mpeg2(data: &[u8]) -> u32 {
    restream_mpegts::wire::crc32_mpeg2(data)
}
#[cfg(test)]
pub use restream_core::{AudioMeta, MediaType, VideoMeta};
#[cfg(test)]
pub use restream_mpegts::demux;
#[cfg(test)]
pub use restream_mpegts::demux::*;
#[cfg(test)]
pub use restream_mpegts::probe as mpegts_probe;
#[cfg(test)]
pub use restream_mpegts::probe::*;
#[cfg(test)]
pub use restream_mpegts::wire;
#[cfg(test)]
pub use restream_mpegts::wire::*;
#[cfg(test)]
#[path = "mpegts_tests.rs"]
mod tests;
