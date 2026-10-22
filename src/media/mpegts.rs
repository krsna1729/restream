//! Low-level MPEG-TS muxing, demuxing, and metadata extraction shared by
//! ingest, HLS, recording, and transcoding paths.

use bytes::Bytes;

use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::MediaType;

mod demux;
#[path = "mpegts_probe.rs"]
mod mpegts_probe;
mod mux;
mod wire;

pub(crate) use demux::TopologyChanges;
pub use demux::{DemuxProbe, TsDemuxer};
pub use mux::{MuxStreamConfig, PacketMeta, TsMuxer, TsSegmentView, TsServiceMetadata};

use demux::StreamKind;

/// The video metadata of an HEVC stream from Annex B bytes that carry its
/// SPS, read by the same parser a TS input's HEVC probe uses, so an fMP4
/// HEVC input describes itself exactly as a TS one does.
pub(crate) fn hevc_video_meta(annex_b: &[u8]) -> VideoMeta {
    let mut meta = mpegts_probe::probe_video(StreamKind::H265, 0, None, None, annex_b);
    meta.pid = None;
    meta
}

/// The H.264 counterpart of [`hevc_video_meta`]: what a TS input's H.264
/// probe reports from Annex B bytes that carry the SPS.
pub(crate) fn h264_video_meta(annex_b: &[u8]) -> VideoMeta {
    let mut meta = mpegts_probe::probe_video(StreamKind::H264, 0, None, None, annex_b);
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
use demux::{
    CC_UNSET, MAX_PES_BUFFER, PMT_VER_UNSET, PesAccumulator, StreamInfo, find_ts_sync,
    ts_sync_candidate_is_valid,
};
#[cfg(test)]
use wire::{
    CRC32_TABLE, SDT_PID, TS_PACKET_SIZE, TS_SYNC_BYTE, crc32_mpeg2, ms_to_ts, parse_timestamp,
    ts_to_ms, write_pcr, write_timestamp,
};

#[cfg(test)]
pub(crate) fn test_crc32_mpeg2(data: &[u8]) -> u32 {
    wire::crc32_mpeg2(data)
}

#[cfg(test)]
#[path = "mpegts_tests.rs"]
mod tests;
