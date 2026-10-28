use crate::domain::output_spec::VideoCodecKind;
use crate::media::engine::MediaEngine;
use crate::media::metadata::AudioMeta;
use crate::media::ring_buffer::RingBuffer;
use rml_rtmp::sessions::StreamMetadata;
use std::sync::Arc;

pub(crate) async fn resolved_output_audio_tracks(
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
            let tracks = crate::sync::lock(&ingest.audio_tracks);
            if !tracks.is_empty() {
                tracks.as_ref().clone()
            } else {
                metadata.audio.into_iter().collect()
            }
        })
        .await
        .unwrap_or_default()
}

pub(crate) async fn output_ring_video_codec_kind(
    engine: &MediaEngine,
    pipeline_id: &str,
    ring_buffer: &Arc<RingBuffer>,
) -> VideoCodecKind {
    let output_codec = ring_buffer.codec_hint_str();
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

pub(crate) async fn rtmp_publish_metadata(
    engine: &MediaEngine,
    pipeline_id: &str,
    output_ring: &Arc<RingBuffer>,
    output_audio_track: Option<&AudioMeta>,
) -> Option<StreamMetadata> {
    let video = engine
        .with_active_ingest(pipeline_id, |ingest| ingest.metadata().video)
        .await
        .flatten();
    restream_rtmp::build_rtmp_stream_metadata(video.as_ref(), output_ring, output_audio_track)
}
