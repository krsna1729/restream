use std::sync::Arc;

use restream::media::avio::MemoryQueue;
use restream::media::metadata::VideoMeta;
use restream::media::mpegts::{TsDemuxer, TsMuxer};
use restream::media::packet::{MediaPacket, MediaType};
use restream::media::ring_buffer::{Reader, RingBuffer};
use restream::media::transcoder::run_ffmpeg_transcode_with_scale;
use tokio_util::sync::CancellationToken;

pub(super) fn synthetic_video_only_ts(fixture: &[u8]) -> Vec<u8> {
    synthetic_video_only_ts_limited(fixture, usize::MAX)
}

pub(super) fn synthetic_video_only_ts_limited(fixture: &[u8], max_video_packets: usize) -> Vec<u8> {
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(fixture);
    let mut all_packets = Vec::new();
    demuxer.drain_into(&mut all_packets);

    let video_meta = VideoMeta {
        codec: "h264".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    let mut muxer = TsMuxer::new(Some(&video_meta), &[]);
    let mut synthetic_ts = Vec::new();

    let mut video_count = 0usize;
    for packet in all_packets
        .into_iter()
        .filter(|packet| packet.media_type == MediaType::Video)
    {
        if video_count >= max_video_packets {
            break;
        }
        video_count += 1;
        let ts_bytes = muxer.mux_packet(
            MediaType::Video,
            0,
            packet.pts,
            packet.dts,
            packet.is_keyframe,
            &packet.payload,
        );
        synthetic_ts.extend_from_slice(ts_bytes);
    }

    assert!(video_count > 0, "fixture must contain video packets");
    synthetic_ts
}

pub(super) fn run_internal_scale_stage(synthetic_ts: &[u8], preset: &str) -> Vec<Arc<MediaPacket>> {
    let input = Arc::new(MemoryQueue::new());
    let output = Arc::new(RingBuffer::new(4096));
    {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(input.write(synthetic_ts));
    }
    input.close();

    let result =
        run_ffmpeg_transcode_with_scale(input, output.clone(), preset, CancellationToken::new());
    assert!(
        result.is_ok(),
        "run_ffmpeg_transcode_with_scale failed for {preset}: {result:?}"
    );

    let mut reader = Reader::new(format!("test_transcode_scale_{preset}"), output);
    let mut output_packets = Vec::new();
    while let Ok(Some(packet)) = reader.pull() {
        output_packets.push(packet);
    }
    output_packets
}
