//! Stream probes for MPEG-TS ingest (codec parameters from the first PES
//! payloads). Untrusted publisher bytes: no indexing, slicing, unwrap or
//! unchecked arithmetic here or in the H.264/H.265 submodules.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::StreamKind;
use crate::media::metadata::{AudioMeta, VideoMeta};

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
                let sps = crate::media::codec::rbsp(raw_sps);
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

    if kind == StreamKind::AacAdts
        && let Some(&[0xFF, sync_low, b2, b3, _, _, _]) = pes_payload.first_chunk::<7>()
    {
        // ADTS header parsing
        if sync_low & 0xF0 == 0xF0 {
            let profile_idx = b2 >> 6;
            meta.profile = match profile_idx {
                0 => Some("Main".to_string()),
                1 => Some("LC".to_string()),
                2 => Some("SSR".to_string()),
                3 => Some("LTP/Reserved".to_string()),
                _ => None,
            };
            let sample_rate_idx = usize::from((b2 >> 2) & 0x0F);
            // 3-bit channel configuration: the low bit of byte 2, then the
            // top two bits of byte 3.
            let channel_config = (u16::from_be_bytes([b2, b3]) >> 6) & 0x07;

            const SAMPLE_RATES: [u32; 13] = [
                96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
                7350,
            ];

            if let Some(&rate) = SAMPLE_RATES.get(sample_rate_idx) {
                meta.sample_rate = rate;
            }
            meta.channels = u32::from(channel_config);
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
    let mut starts = starts.iter().peekable();
    while let Some(&(_, nalu_start)) = starts.next() {
        let nalu_end = starts
            .peek()
            .map_or(data.len(), |&&(next_code, _)| next_code);
        if let Some(nalu) = data.get(nalu_start..nalu_end)
            && !nalu.is_empty()
            && callback(nalu)
        {
            return true;
        }
    }
    false
}
