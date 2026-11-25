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

use std::ops::ControlFlow;

use restream_core::{AudioMeta, MediaType, VideoMeta};

mod h264;
mod h265;

const STREAM_TYPE_H264: u8 = 0x1B;
const STREAM_TYPE_H265: u8 = 0x24;
const STREAM_TYPE_AAC_ADTS: u8 = 0x0F;
const STREAM_TYPE_AAC_LATM: u8 = 0x11;

/// Elementary stream kind carried by one MPEG-TS PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    H264,
    H265,
    AacAdts,
    AacLatm,
}

impl StreamKind {
    pub(crate) fn from_stream_type(stream_type: u8) -> Option<Self> {
        match stream_type {
            STREAM_TYPE_H264 => Some(Self::H264),
            STREAM_TYPE_H265 => Some(Self::H265),
            STREAM_TYPE_AAC_ADTS => Some(Self::AacAdts),
            STREAM_TYPE_AAC_LATM => Some(Self::AacLatm),
            _ => None,
        }
    }

    pub(crate) fn media_type(self) -> MediaType {
        match self {
            Self::H264 | Self::H265 => MediaType::Video,
            Self::AacAdts | Self::AacLatm => MediaType::Audio,
        }
    }

    pub fn codec_name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
            Self::AacAdts | Self::AacLatm => "aac",
        }
    }
}

use h264::parse_sps as parse_h264_sps;
pub use h264::{find_sps as find_h264_sps, is_keyframe as h264_is_keyframe};
pub use h265::{
    find_sps as find_h265_sps, is_keyframe as h265_is_keyframe, parse_sps as parse_h265_sps,
};

pub fn probe_video(
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
                let sps = restream_core::rbsp(raw_sps);
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

pub fn video_meta_complete(kind: StreamKind, meta: &VideoMeta) -> bool {
    match kind {
        StreamKind::H264 | StreamKind::H265 => meta.width > 0 && meta.height > 0,
        StreamKind::AacAdts | StreamKind::AacLatm => true,
    }
}

pub fn probe_audio(
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

            if let Some(&rate) = restream_core::AAC_SAMPLE_RATES.get(sample_rate_idx) {
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

pub fn audio_meta_complete(kind: StreamKind, meta: &AudioMeta) -> bool {
    match kind {
        StreamKind::AacAdts => meta.sample_rate > 0 && meta.channels > 0,
        StreamKind::AacLatm | StreamKind::H264 | StreamKind::H265 => true,
    }
}

/// Whether `callback` holds for some NAL unit (header included) of an Annex B
/// payload. Runs per video PES on the SRT ingress Owner, so it walks the
/// start codes in place rather than collecting them.
fn for_each_nal_raw<F>(data: &[u8], mut callback: F) -> bool
where
    F: FnMut(&[u8]) -> bool,
{
    restream_core::for_each_annexb_nalu(data, |nalu| {
        if callback(nalu) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}
