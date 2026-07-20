//! RTMP egress startup headers and packet policy.

#[cfg(test)]
use std::sync::Arc;

use bytes::Bytes;

use crate::media::codec;
use crate::media::metadata::AudioMeta;
use crate::media::ring_buffer::RingBuffer;

use super::enhanced::hevc_sequence_header_for_keyframe;
use super::flv::{FlvVideoPacketKind, classify_flv_video_packet};

pub(super) fn cache_h264_parameter_sets(payload: &[u8], cache: &mut Vec<u8>) {
    let Some(parameter_sets) = codec::annexb_parameter_sets(payload) else {
        return;
    };
    if h264_sps_nalu(&parameter_sets).is_some() {
        *cache = parameter_sets;
    }
}

pub(super) fn startup_video_sequence_header(
    ring_buffer: &RingBuffer,
    ingest_sequence_header: Option<Bytes>,
    enhanced_hevc_video: bool,
) -> Option<Bytes> {
    if let Some(parameter_sets) = ring_buffer.video_parameter_sets()
        && let Some(sequence_header) = if enhanced_hevc_video {
            codec::build_hevc_enhanced_rtmp_sequence_header(&parameter_sets)
        } else {
            codec::build_avcc_sequence_header(&parameter_sets)
        }
    {
        return Some(sequence_header);
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

    ingest_sequence_header
}

pub(super) fn rtmp_output_waits_for_video(ring_buffer: &RingBuffer) -> bool {
    !ring_buffer.codec_hint_str().is_empty() || ring_buffer.video_parameter_sets().is_some()
}

pub(super) fn rtmp_video_packet_can_be_dropped(payload: &[u8], is_keyframe: bool) -> bool {
    !is_keyframe
        && matches!(
            classify_flv_video_packet(payload),
            Some(FlvVideoPacketKind::Interframe)
        )
}

#[cfg(test)]
pub(super) fn rtmp_warmup_ready(
    ring_buffer: &RingBuffer,
    packets: &[Arc<crate::media::packet::MediaPacket>],
) -> bool {
    !rtmp_output_waits_for_video(ring_buffer)
        || ring_buffer.video_parameter_sets().is_some()
        || packets
            .iter()
            .any(|packet| packet.media_type == crate::media::packet::MediaType::Video)
}

pub(super) fn should_send_startup_audio_sequence_header(
    video_ready: bool,
    ring_buffer: &RingBuffer,
) -> bool {
    video_ready
        || !rtmp_output_waits_for_video(ring_buffer)
        || ring_buffer.video_parameter_sets().is_some()
}

pub(super) fn should_defer_audio_until_video_ready(
    video_ready: bool,
    ring_buffer: &RingBuffer,
) -> bool {
    !video_ready && rtmp_output_waits_for_video(ring_buffer)
}

pub(super) fn resolve_deferred_audio_sequence_header(
    cached_sequence_header: Option<&Bytes>,
    output_audio_track: Option<&AudioMeta>,
) -> Option<Bytes> {
    cached_sequence_header.cloned().or_else(|| {
        output_audio_track.and_then(|track| {
            track
                .codec
                .eq_ignore_ascii_case("aac")
                .then(|| codec::build_aac_sequence_header(track.sample_rate, track.channels))
        })
    })
}

pub(super) fn h264_sps_nalu(payload: &[u8]) -> Option<Vec<u8>> {
    codec::split_annexb_nalus(payload)
        .iter()
        .find(|nalu| !nalu.is_empty() && (nalu[0] & 0x1F) == 7)
        .map(|nalu| nalu.to_vec())
}

pub(super) fn h264_sequence_header_for_keyframe(
    payload: &[u8],
    parameter_sets_cache: &[u8],
) -> Option<(Bytes, Option<Vec<u8>>)> {
    let sequence_header = codec::build_avcc_sequence_header(payload)
        .or_else(|| codec::build_avcc_sequence_header(parameter_sets_cache))?;
    let sps = h264_sps_nalu(payload).or_else(|| h264_sps_nalu(parameter_sets_cache));
    Some((sequence_header, sps))
}

pub(super) fn video_sequence_header_for_keyframe(
    enhanced_hevc_video: bool,
    payload: &[u8],
    parameter_sets_cache: &[u8],
) -> Option<(Bytes, Option<Vec<u8>>)> {
    if enhanced_hevc_video {
        hevc_sequence_header_for_keyframe(payload, parameter_sets_cache)
    } else {
        h264_sequence_header_for_keyframe(payload, parameter_sets_cache)
    }
}

pub(super) fn validate_rtmp_output_audio_packet_track(track_index: u32) -> Result<(), String> {
    if track_index != 0 {
        return Err(format!(
            "RTMP output requires a single routed audio track, but observed track index {} on the output ring. Choose subset, downmix, or remap audio routing.",
            track_index
        ));
    }
    Ok(())
}
