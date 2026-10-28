//! FLV tag probes for RTMP ingest and egress packaging.
//! Every input here is untrusted publisher bytes, so the module may not index,
//! slice, unwrap or do unchecked integer arithmetic: a malformed tag is `None`,
//! never a panic or a wrapped value.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use restream_core::{
    AAC_SAMPLE_RATES, AudioMeta, VideoMeta, annexb_parameter_sets, level_name, parse_h264_sps,
    profile_name, signed_be24, take_u16_prefixed,
};
pub use restream_core::{AvccRecord, avcc_record};

use crate::enhanced_rtmp_hevc::{
    EX_CODED_FRAMES, EX_CODED_FRAMES_X, EX_SEQUENCE_START, ExVideoTag, ex_video_tag, hvcc_record,
    parse_hevc_profile_tier_level,
};

pub fn parse_flv_video_meta(data: &[u8]) -> Option<VideoMeta> {
    if let Some(tag) = ex_video_tag(data) {
        return Some(enhanced_video_meta(&tag));
    }
    let &[tag, packet_type] = data.first_chunk::<2>()?;
    let codec_id = tag & 0x0F;
    let codec = match codec_id {
        7 => "h264",
        12 => "h265",
        13 => "av1",
        2 => "h263",
        4 => "vp6",
        _ => return None,
    };

    let mut meta = VideoMeta {
        codec: codec.to_string(),
        ..Default::default()
    };

    // For H.264: byte[1]=AVC packet type, bytes[5..] = AVCDecoderConfigurationRecord when type=0
    if codec_id == 7 && packet_type == 0 && data.len() > 12 {
        let avc_config = data.get(5..)?;
        if let Some(&[_, profile_idc, _, level_idc]) = avc_config.first_chunk::<4>() {
            meta.profile = Some(profile_name(profile_idc).to_string());
            meta.level = Some(level_name(level_idc));

            // Parse the first SPS for resolution and timing info. Only the
            // first SPS is read, so a record whose PPS list is truncated still
            // reports its size (`avcc_record` would reject the whole record).
            if let Some((&num_sps, mut sps_list)) =
                avc_config.get(5..).and_then(<[u8]>::split_first)
                && num_sps & 0x1F > 0
                && let Some(sps) = take_u16_prefixed(&mut sps_list)
                && let Some((_nal_header, payload)) = sps.split_first()
                && let Some(info) = parse_h264_sps(payload)
            {
                meta.width = info.width;
                meta.height = info.height;
                meta.fps = info.fps;
            }
        }
    }

    Some(meta)
}

/// An enhanced RTMP (`avc1`/`hvc1`) publish: the codec, and from a
/// sequence start the picture size in its SPS.
pub fn enhanced_video_meta(tag: &ExVideoTag<'_>) -> VideoMeta {
    let mut meta = VideoMeta {
        codec: if tag.is_hevc() { "h265" } else { "h264" }.to_string(),
        ..Default::default()
    };
    if tag.packet_type != EX_SEQUENCE_START {
        return meta;
    }
    if tag.is_hevc() {
        if let Some(record) = hvcc_record(tag.body)
            && let Some(sps) = record.sps.first()
            && let Some(info) = parse_hevc_profile_tier_level(sps)
        {
            meta.width = info.width;
            meta.height = info.height;
        }
    } else if let Some(record) = avcc_record(tag.body)
        && let Some((_nal_header, payload)) = record.sps.first().and_then(|sps| sps.split_first())
        && let Some(info) = parse_h264_sps(payload)
    {
        meta.width = info.width;
        meta.height = info.height;
        meta.fps = info.fps;
    }
    meta
}

/// The parameter sets of an FLV video sequence header (classic H.264, or
/// an enhanced RTMP `avc1`/`hvc1` sequence start) in Annex-B form, the
/// format `RingBuffer::video_parameter_sets` callers expect. Returns `None`
/// for anything malformed or another codec — this parses untrusted
/// publisher input, so it must never panic.
pub fn flv_config_annexb_parameter_sets(data: &[u8]) -> Option<Vec<u8>> {
    let (_, annexb) = flv_video_config(data)?;
    annexb_parameter_sets(&annexb)
}

pub fn flv_video_config(payload: &[u8]) -> Option<(usize, Vec<u8>)> {
    let (length_size_minus_one, sets): (u8, Vec<&[u8]>) = match ex_video_tag(payload) {
        Some(tag) if tag.packet_type != EX_SEQUENCE_START => return None,
        Some(tag) if tag.is_hevc() => {
            let record = hvcc_record(tag.body)?;
            if record.vps.is_empty() || record.sps.is_empty() || record.pps.is_empty() {
                return None;
            }
            let sets = [record.vps, record.sps, record.pps].concat();
            (record.length_size_minus_one, sets)
        }
        Some(tag) => {
            let record = avcc_record(tag.body)?;
            if record.sps.is_empty() || record.pps.is_empty() {
                return None;
            }
            (
                record.length_size_minus_one,
                [record.sps, record.pps].concat(),
            )
        }
        None => {
            let (&[tag, 0, ..], body) = payload.split_first_chunk::<5>()? else {
                return None;
            };
            match tag & 0x0F {
                7 => {
                    let record = avcc_record(body)?;
                    if record.sps.is_empty() || record.pps.is_empty() {
                        return None;
                    }
                    (
                        record.length_size_minus_one,
                        [record.sps, record.pps].concat(),
                    )
                }
                12 => {
                    let record = hvcc_record(body)?;
                    if record.vps.is_empty() || record.sps.is_empty() || record.pps.is_empty() {
                        return None;
                    }
                    let sets = [record.vps, record.sps, record.pps].concat();
                    (record.length_size_minus_one, sets)
                }
                _ => return None,
            }
        }
    };
    let mut annexb = Vec::new();
    for nalu in sets {
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(nalu);
    }
    Some((usize::from(length_size_minus_one).saturating_add(1), annexb))
}

pub fn flv_video_composition_time_ms(data: &[u8]) -> i32 {
    if let Some(tag) = ex_video_tag(data) {
        return tag.composition_time_ms;
    }
    let Some(&[tag, packet_type, high, mid, low]) = data.first_chunk::<5>() else {
        return 0;
    };
    if !matches!(tag & 0x0f, 7 | 12) || packet_type != 1 {
        return 0;
    }
    // Signed 24-bit big-endian: extend the sign into the top byte.
    let sign = if high & 0x80 != 0 { 0xFF } else { 0x00 };
    i32::from_be_bytes([sign, high, mid, low])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlvVideoPacketKind {
    SequenceHeader,
    Keyframe,
    Interframe,
}

pub fn classify_flv_video_packet(data: &[u8]) -> Option<FlvVideoPacketKind> {
    if let Some(tag) = ex_video_tag(data) {
        return match tag.packet_type {
            EX_SEQUENCE_START => Some(FlvVideoPacketKind::SequenceHeader),
            EX_CODED_FRAMES | EX_CODED_FRAMES_X if tag.keyframe => {
                Some(FlvVideoPacketKind::Keyframe)
            }
            EX_CODED_FRAMES | EX_CODED_FRAMES_X => Some(FlvVideoPacketKind::Interframe),
            // Sequence end, metadata: no picture.
            _ => None,
        };
    }
    let &[tag, packet_type] = data.first_chunk::<2>()?;
    if !matches!(tag & 0x0f, 7 | 12) {
        return None;
    }

    if packet_type == 0 {
        return Some(FlvVideoPacketKind::SequenceHeader);
    }

    Some(if (tag >> 4) == 1 {
        FlvVideoPacketKind::Keyframe
    } else {
        FlvVideoPacketKind::Interframe
    })
}

pub fn parse_flv_audio_meta(data: &[u8]) -> Option<AudioMeta> {
    let (&byte0, rest) = data.split_first()?;
    let format_id = (byte0 >> 4) & 0x0F;
    let rate_id = (byte0 >> 2) & 0x03;
    let channels_id = byte0 & 0x01;

    let codec = match format_id {
        10 => "aac",
        2 => "mp3",
        11 => "speex",
        14 => "mp3-8k",
        0 => "pcm",
        1 => "adpcm",
        _ => "unknown",
    };

    let sample_rate = match rate_id {
        0 => 5500,
        1 => 11025,
        2 => 22050,
        3 => 44100,
        _ => 0,
    };
    let channels = if channels_id == 1 { 2 } else { 1 };

    let mut meta = AudioMeta {
        codec: codec.to_string(),
        sample_rate,
        channels,
        channel_layout: Some(if channels == 1 { "mono" } else { "stereo" }.to_string()),
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };

    // AAC AudioSpecificConfig gives actual sample rate/channels
    if format_id == 10
        && let Some((&0, asc)) = rest.split_first()
        && let Some(&[asc0, asc1]) = asc.first_chunk::<2>()
    {
        {
            let audio_object_type = asc0 >> 3;
            meta.profile = match audio_object_type {
                1 => Some("Main".to_string()),
                2 => Some("LC".to_string()),
                3 => Some("SSR".to_string()),
                4 => Some("LTP".to_string()),
                5 => Some("SBR".to_string()),
                _ => Some(format!("AAC Profile {}", audio_object_type)),
            };
            // 5 bits object type, 4 bits frequency index, 4 bits channels.
            let freq_idx = (u16::from_be_bytes([asc0, asc1]) >> 7) & 0x0F;
            let ch_config = (asc1 >> 3) & 0x0F;
            if let Some(&rate) = AAC_SAMPLE_RATES.get(usize::from(freq_idx)) {
                meta.sample_rate = rate;
            }
            if ch_config > 0 {
                // Channel configuration 7 is 7.1: eight channels (ISO/IEC
                // 14496-3 Table 1.19), as the MPEG-TS ADTS probe reports.
                meta.channels = if ch_config == 7 {
                    8
                } else {
                    u32::from(ch_config)
                };
                meta.channel_layout = Some(
                    match ch_config {
                        1 => "mono",
                        2 => "stereo",
                        3 => "3.0",
                        4 => "4.0",
                        5 => "5.0",
                        6 => "5.1",
                        7 => "7.1",
                        _ => "unknown",
                    }
                    .to_string(),
                );
            }
        }
    }

    Some(meta)
}

pub fn strip_adts(payload: &[u8]) -> &[u8] {
    if let [0xFF, second, _, _, _, _, _, ..] = *payload
        && second & 0xF0 == 0xF0
    {
        let hdr_len = if second & 0x01 == 1 { 7 } else { 9 };
        if payload.len() > hdr_len {
            return payload.get(hdr_len..).unwrap_or(payload);
        }
    }
    payload
}

pub fn audio_for_rtmp_into(payload: &[u8], out: &mut Vec<u8>) {
    let raw = strip_adts(payload);
    out.clear();
    out.reserve(raw.len().saturating_add(2));
    out.extend_from_slice(&[0xAF, 0x01]);
    out.extend_from_slice(raw);
}

pub use restream_core::codec::annexb_to_avcc_into;

pub fn video_for_rtmp_with_composition_into(
    payload: &[u8],
    is_keyframe: bool,
    composition_time_ms: i32,
    out: &mut Vec<u8>,
) -> bool {
    let tag = if is_keyframe { 0x17u8 } else { 0x27u8 };
    out.clear();
    out.extend_from_slice(&[tag, 1]);
    out.extend_from_slice(&signed_be24(composition_time_ms));
    annexb_to_avcc_into(payload, out)
}

pub fn video_for_rtmp_into(payload: &[u8], is_keyframe: bool, out: &mut Vec<u8>) -> bool {
    video_for_rtmp_with_composition_into(payload, is_keyframe, 0, out)
}
