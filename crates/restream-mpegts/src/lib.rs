//! Low-level MPEG-TS muxing, demuxing, and metadata extraction shared by
//! ingest, HLS, recording, and transcoding paths.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

use bytes::Bytes;
use restream_core::{AudioMeta, MediaType, VideoMeta};

mod demux;
mod mux;
mod probe;
pub mod wire;

pub use demux::{DemuxProbe, StreamKind, TopologyChanges, TsDemuxer};
pub use mux::{MuxStreamConfig, PacketMeta, TsMuxer, TsSegmentView, TsServiceMetadata};

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

pub fn remux_segment_view(
    segment: &[u8],
    video: Option<&VideoMeta>,
    audio_tracks: &[AudioMeta],
    view: TsSegmentView,
) -> Option<Bytes> {
    let (mux_video, mux_audio) = match view {
        TsSegmentView::Video => (video, Vec::new()),
        TsSegmentView::Audio(track_index) => {
            let audio = audio_tracks
                .iter()
                .find(|track| track.track_index == track_index)
                .cloned()?;
            (None, vec![audio])
        }
    };

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(segment);
    demuxer.flush();

    let mut muxer = TsMuxer::new(mux_video, &mux_audio);
    let mut output = Vec::with_capacity(segment.len().min(256 * 1024));
    let mut wrote_media = false;

    for packet in demuxer.drain() {
        let include = match view {
            TsSegmentView::Video => packet.media_type == MediaType::Video,
            TsSegmentView::Audio(track_index) => {
                packet.media_type == MediaType::Audio && packet.track_index == track_index
            }
        };
        if !include {
            continue;
        }

        let data = muxer.mux_packet(
            packet.media_type,
            packet.track_index,
            packet.pts,
            packet.dts,
            packet.is_keyframe,
            &packet.payload,
        );
        if !data.is_empty() {
            wrote_media = true;
            output.extend_from_slice(data);
        }
    }

    wrote_media.then(|| Bytes::from(output))
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../src/test_fixtures/paths.rs"]
mod test_fixtures;

#[cfg(test)]
#[path = "mpegts_tests.rs"]
mod tests;
