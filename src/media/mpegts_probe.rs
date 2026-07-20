use super::StreamKind;
use crate::media::metadata::{AudioMeta, VideoMeta};

#[path = "mpegts_probe/bit_reader.rs"]
mod bit_reader;
#[path = "mpegts_probe/h264.rs"]
mod h264;
#[path = "mpegts_probe/h265.rs"]
mod h265;

use h264::parse_sps as parse_h264_sps;
pub(super) use h264::{find_sps as find_h264_sps, is_keyframe as h264_is_keyframe};
pub(super) use h265::{
    find_sps as find_h265_sps, is_keyframe as h265_is_keyframe, parse_sps as parse_h265_sps,
};

pub(super) fn probe_video(
    kind: StreamKind,
    pid: u16,
    language: Option<String>,
    title: Option<String>,
    pes_payload: &[u8],
) -> VideoMeta {
    let mut meta = VideoMeta {
        codec: kind.codec_name().to_string(),
        width: 0,
        height: 0,
        fps: 0.0,
        bw: None,
        pid: Some(pid),
        language,
        title,
        profile: None,
        level: None,
        pixel_format: None,
    };

    let mut parsed_meta = meta.clone();
    let parsed = match kind {
        StreamKind::H264 => {
            if let Some(ref sps) = find_h264_sps(pes_payload) {
                parse_h264_sps(sps, &mut parsed_meta).is_some()
            } else {
                false
            }
        }
        StreamKind::H265 => {
            if let Some(ref raw_sps) = find_h265_sps(pes_payload) {
                let sps = remove_emulation_prevention(raw_sps);
                parse_h265_sps(&sps, &mut parsed_meta).is_some()
            } else {
                false
            }
        }
        _ => false,
    };
    if parsed {
        meta = parsed_meta;
    }

    meta
}

pub(super) fn video_meta_complete(kind: StreamKind, meta: &VideoMeta) -> bool {
    match kind {
        StreamKind::H264 | StreamKind::H265 => meta.width > 0 && meta.height > 0,
        StreamKind::AacAdts | StreamKind::AacLatm => true,
    }
}

pub(super) fn probe_audio(
    kind: StreamKind,
    track_index: u32,
    pid: u16,
    language: Option<String>,
    title: Option<String>,
    pes_payload: &[u8],
) -> AudioMeta {
    let mut meta = AudioMeta {
        codec: kind.codec_name().to_string(),
        sample_rate: 0,
        channels: 0,
        channel_layout: None,
        track_index,
        pid: Some(pid),
        language,
        title,
        profile: None,
    };

    if kind == StreamKind::AacAdts && pes_payload.len() >= 7 {
        // ADTS header parsing
        if pes_payload[0] == 0xFF && (pes_payload[1] & 0xF0) == 0xF0 {
            let profile_idx = (pes_payload[2] >> 6) as usize;
            meta.profile = match profile_idx {
                0 => Some("Main".to_string()),
                1 => Some("LC".to_string()),
                2 => Some("SSR".to_string()),
                3 => Some("LTP/Reserved".to_string()),
                _ => None,
            };
            let sample_rate_idx = ((pes_payload[2] >> 2) & 0x0F) as usize;
            let channel_config = ((pes_payload[2] & 0x01) << 2) | ((pes_payload[3] >> 6) & 0x03);

            const SAMPLE_RATES: [u32; 13] = [
                96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
                7350,
            ];

            if sample_rate_idx < SAMPLE_RATES.len() {
                meta.sample_rate = SAMPLE_RATES[sample_rate_idx];
            }
            meta.channels = channel_config as u32;
            if meta.channels == 7 {
                meta.channels = 8;
            }
        }
    }

    meta
}

pub(super) fn audio_meta_complete(kind: StreamKind, meta: &AudioMeta) -> bool {
    match kind {
        StreamKind::AacAdts => meta.sample_rate > 0 && meta.channels > 0,
        StreamKind::AacLatm | StreamKind::H264 | StreamKind::H265 => true,
    }
}

/// Raw Annex B start-code scanner. Callback receives full NAL data (including header).
fn for_each_nal_raw<F>(data: &[u8], mut callback: F) -> bool
where
    F: FnMut(&[u8]) -> bool,
{
    let starts = crate::media::codec::find_annexb_start_codes(data);
    if starts.is_empty() {
        return false;
    }
    for i in 0..starts.len() {
        let nalu_start = starts[i].1;
        let nalu_end = if i + 1 < starts.len() {
            starts[i + 1].0
        } else {
            data.len()
        };
        if nalu_start < nalu_end && callback(&data[nalu_start..nalu_end]) {
            return true;
        }
    }
    false
}

/// Remove RBSP emulation prevention bytes (0x00 0x00 0x03 → 0x00 0x00).
fn remove_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 3 {
            out.push(0);
            out.push(0);
            i += 3;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}
