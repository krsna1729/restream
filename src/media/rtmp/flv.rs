//! FLV tag probes for RTMP ingest. Every input here is untrusted publisher
//! bytes, so the module may not index, slice, unwrap or do unchecked integer
//! arithmetic: a malformed tag is `None`, never a panic or a wrapped value.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use crate::media::codec;
use crate::media::metadata::{AudioMeta, VideoMeta};

/// The next `u16` length-prefixed item of an AVCDecoderConfigurationRecord
/// list, advancing `rest` past it.
fn take_u16_prefixed<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, tail) = rest.split_first_chunk::<2>()?;
    let (item, tail) = tail.split_at_checked(usize::from(u16::from_be_bytes(*len)))?;
    *rest = tail;
    Some(item)
}

pub(super) fn parse_flv_video_meta(data: &[u8]) -> Option<VideoMeta> {
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
            meta.profile = Some(codec::profile_name(profile_idc).to_string());
            meta.level = Some(codec::level_name(level_idc));

            // Parse the first SPS for resolution and timing info.
            if let Some((&num_sps, mut sps_list)) =
                avc_config.get(5..).and_then(<[u8]>::split_first)
                && num_sps & 0x1F > 0
                && let Some(sps) = take_u16_prefixed(&mut sps_list)
                && let Some((_nal_header, payload)) = sps.split_first()
                && let Some(info) = codec::parse_h264_sps(payload)
            {
                meta.width = info.width;
                meta.height = info.height;
                meta.fps = info.fps;
            }
        }
    }

    Some(meta)
}

/// Extracts SPS/PPS from an FLV AVCDecoderConfigurationRecord (the H.264
/// sequence-header video tag body) and re-emits them in Annex-B form
/// (start-code prefixed), the format `RingBuffer::video_parameter_sets`
/// callers expect. Returns `None` for anything malformed or non-H.264 —
/// this parses untrusted publisher input, so it must never panic.
pub(super) fn flv_avcc_config_annexb_parameter_sets(data: &[u8]) -> Option<Vec<u8>> {
    let &[tag, packet_type] = data.first_chunk::<2>()?;
    if tag & 0x0F != 7 || packet_type != 0 {
        return None;
    }
    // Five FLV tag bytes, then the record; its sixth byte is the SPS count.
    let (&num_sps, mut rest) = data.get(10..)?.split_first()?;

    let mut annexb = Vec::new();
    for _ in 0..(num_sps & 0x1F) {
        let sps = take_u16_prefixed(&mut rest)?;
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(sps);
    }
    let (&num_pps, tail) = rest.split_first()?;
    rest = tail;
    for _ in 0..num_pps {
        let pps = take_u16_prefixed(&mut rest)?;
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(pps);
    }

    codec::annexb_parameter_sets(&annexb)
}

pub(super) fn flv_video_composition_time_ms(data: &[u8]) -> i32 {
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
pub(super) enum FlvVideoPacketKind {
    SequenceHeader,
    Keyframe,
    Interframe,
}

pub(super) fn classify_flv_video_packet(data: &[u8]) -> Option<FlvVideoPacketKind> {
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

pub(super) fn parse_flv_audio_meta(data: &[u8]) -> Option<AudioMeta> {
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
            let aac_rates: &[u32] = &[
                96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
                7350,
            ];
            if let Some(&rate) = aac_rates.get(usize::from(freq_idx)) {
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
