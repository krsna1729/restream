//! RTMP egress startup headers and packet policy.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use bytes::Bytes;
use restream_core::{AudioMeta, build_aac_sequence_header, build_avcc_sequence_header};
use restream_sync::RingBuffer;

use crate::enhanced_rtmp_hevc::build_hevc_enhanced_rtmp_sequence_header;
use crate::flv::{FlvVideoPacketKind, classify_flv_video_packet};

pub fn video_sequence_header(enhanced_hevc_video: bool, parameter_sets: &[u8]) -> Option<Bytes> {
    if enhanced_hevc_video {
        build_hevc_enhanced_rtmp_sequence_header(parameter_sets)
    } else {
        build_avcc_sequence_header(parameter_sets)
    }
}

/// The startup video sequence header and the ring configuration generation
/// it was built from (0 when it is the ingest-cached header).
pub fn startup_video_sequence_header(
    ring_buffer: &RingBuffer,
    ingest_sequence_header: Option<Bytes>,
    enhanced_hevc_video: bool,
) -> Option<(Bytes, u32)> {
    if let Some(config) = ring_buffer.video_config()
        && let Some(sequence_header) = video_sequence_header(enhanced_hevc_video, &config.bytes)
    {
        return Some((sequence_header, config.generation));
    }

    if enhanced_hevc_video {
        return None;
    }

    // A brand-new raw H.264 output ring can be empty when the RTMP publisher
    // connects before the stage has emitted its first keyframe. In that case
    // the ingest-cached sequence header may describe the source stream rather
    // than the transcode output (for example 1080p source vs 720p stage), so
    // wait for the output ring's own keyframe/config instead of advertising
    // the wrong decoder config.
    if ring_buffer.get_write_idx() == 0 && !ring_buffer.codec_hint_str().is_empty() {
        return None;
    }

    ingest_sequence_header.map(|header| (header, 0))
}

pub fn rtmp_output_waits_for_video(ring_buffer: &RingBuffer) -> bool {
    !ring_buffer.codec_hint_str().is_empty() || ring_buffer.video_parameter_sets().is_some()
}

pub fn rtmp_video_packet_can_be_dropped(payload: &[u8], is_keyframe: bool) -> bool {
    !is_keyframe
        && matches!(
            classify_flv_video_packet(payload),
            Some(FlvVideoPacketKind::Interframe)
        )
}

pub fn should_send_startup_audio_sequence_header(
    video_ready: bool,
    ring_buffer: &RingBuffer,
) -> bool {
    video_ready
        || !rtmp_output_waits_for_video(ring_buffer)
        || ring_buffer.video_parameter_sets().is_some()
}

pub fn should_defer_audio_until_video_ready(video_ready: bool, ring_buffer: &RingBuffer) -> bool {
    !video_ready && rtmp_output_waits_for_video(ring_buffer)
}

pub fn resolve_deferred_audio_sequence_header(
    cached_sequence_header: Option<&Bytes>,
    output_audio_track: Option<&AudioMeta>,
) -> Option<Bytes> {
    cached_sequence_header.cloned().or_else(|| {
        output_audio_track.and_then(|track| {
            track
                .codec
                .eq_ignore_ascii_case("aac")
                .then(|| build_aac_sequence_header(track.sample_rate, track.channels))
        })
    })
}

pub fn validate_rtmp_output_audio_packet_track(track_index: u32) -> Result<(), String> {
    if track_index != 0 {
        return Err(format!(
            "RTMP output requires a single routed audio track, but observed track index {} on the output ring. Choose subset, downmix, or remap audio routing.",
            track_index
        ));
    }
    Ok(())
}
