use restream_core::{AudioMeta, VideoCodecKind, VideoMeta};
use restream_sync::RingBuffer;
use rml_rtmp::sessions::StreamMetadata;

pub const RTMP_METADATA_VIDEO_CODEC_ID_AVC: u32 = 7;
pub const RTMP_METADATA_VIDEO_CODEC_ID_HEVC: u32 = u32::from_be_bytes(*b"hvc1");

pub fn validate_rtmp_output_audio_tracks(audio_tracks: &[AudioMeta]) -> Result<(), String> {
    if audio_tracks.len() > 1 {
        return Err(format!(
            "RTMP output supports exactly one audio track, but this output resolved to {} tracks. Choose subset, downmix, or remap audio routing.",
            audio_tracks.len()
        ));
    }
    Ok(())
}

pub fn build_rtmp_stream_metadata(
    video: Option<&VideoMeta>,
    output_ring: &RingBuffer,
    output_audio_track: Option<&AudioMeta>,
) -> Option<StreamMetadata> {
    let mut metadata = StreamMetadata::new();

    if let Some(video) = video {
        let codec = rtmp_output_video_codec(video, output_ring);
        metadata.video_codec_id = if codec.eq_ignore_ascii_case("h264") {
            Some(RTMP_METADATA_VIDEO_CODEC_ID_AVC)
        } else if VideoCodecKind::from_codec_name(codec).is_hevc() {
            Some(RTMP_METADATA_VIDEO_CODEC_ID_HEVC)
        } else {
            None
        };
        metadata.video_width = (video.width > 0).then_some(video.width);
        metadata.video_height = (video.height > 0).then_some(video.height);
        metadata.video_frame_rate = (video.fps > 0.0).then_some(video.fps as f32);
    }

    if let Some(track) = output_audio_track
        && track.codec.eq_ignore_ascii_case("aac")
    {
        metadata.audio_codec_id = Some(10);
        metadata.audio_sample_rate = Some(track.sample_rate);
        metadata.audio_channels = Some(track.channels);
        metadata.audio_is_stereo = Some(track.channels >= 2);
    }

    (metadata.video_codec_id.is_some() || metadata.audio_codec_id.is_some()).then_some(metadata)
}

pub fn rtmp_output_video_codec<'a>(
    ingest_video: &'a VideoMeta,
    output_ring: &'a RingBuffer,
) -> &'a str {
    let output_codec = output_ring.codec_hint_str();
    if output_codec.is_empty() {
        ingest_video.codec.as_str()
    } else {
        output_codec
    }
}
