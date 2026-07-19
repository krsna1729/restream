use crate::domain::output_spec::VideoCodecKind;
use crate::media::engine::{AudioMeta, MediaEngine, VideoMeta};
use crate::media::ring_buffer::RingBuffer;
use rml_rtmp::sessions::StreamMetadata;
use std::sync::Arc;

const RTMP_METADATA_VIDEO_CODEC_ID_AVC: u32 = 7;
pub(super) const RTMP_METADATA_VIDEO_CODEC_ID_HEVC: u32 = u32::from_be_bytes(*b"hvc1");

pub(super) async fn resolved_output_audio_tracks(
    engine: &MediaEngine,
    pipeline_id: &str,
    ring_buffer: &Arc<RingBuffer>,
) -> Vec<AudioMeta> {
    if let Some(tracks) = ring_buffer.audio_tracks()
        && !tracks.is_empty()
    {
        return tracks.to_vec();
    }

    engine
        .with_active_ingest(pipeline_id, |ingest| {
            let metadata = ingest.metadata();
            let tracks = ingest
                .audio_tracks
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !tracks.is_empty() {
                tracks.as_ref().clone()
            } else {
                metadata.audio.into_iter().collect()
            }
        })
        .await
        .unwrap_or_default()
}

pub(super) async fn output_ring_video_codec_kind(
    engine: &MediaEngine,
    pipeline_id: &str,
    output_ring: &RingBuffer,
) -> VideoCodecKind {
    let output_codec = output_ring.codec_hint_str();
    if !output_codec.is_empty() {
        return VideoCodecKind::from_codec_name(output_codec);
    }

    engine
        .with_active_ingest(pipeline_id, |ingest| {
            ingest
                .metadata()
                .video
                .map(|video| VideoCodecKind::from_codec_name(&video.codec))
        })
        .await
        .flatten()
        .unwrap_or(VideoCodecKind::Unknown)
}

pub(super) fn validate_rtmp_output_audio_tracks(audio_tracks: &[AudioMeta]) -> Result<(), String> {
    if audio_tracks.len() > 1 {
        return Err(format!(
            "RTMP output supports exactly one audio track, but this output resolved to {} tracks. Choose subset, downmix, or remap audio routing.",
            audio_tracks.len()
        ));
    }
    Ok(())
}

pub(super) async fn rtmp_publish_metadata(
    engine: &MediaEngine,
    pipeline_id: &str,
    output_ring: &Arc<RingBuffer>,
    output_audio_track: Option<&AudioMeta>,
) -> Option<StreamMetadata> {
    let video = engine
        .with_active_ingest(pipeline_id, |ingest| ingest.metadata().video)
        .await
        .flatten();
    let mut metadata = StreamMetadata::new();

    if let Some(video) = video {
        let codec = rtmp_output_video_codec(&video, output_ring);
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

fn rtmp_output_video_codec<'a>(
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
