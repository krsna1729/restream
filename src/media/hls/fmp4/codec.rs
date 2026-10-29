#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use std::num::NonZeroU32;

use shiguredo_mp4::{
    TrackKind,
    bitstream::{
        aac::{
            AudioObjectType, AudioSpecificConfig, ChannelConfiguration, Mp4aSampleEntryConfig,
            SamplingFrequency, build_mp4a_box, parse_audio_specific_config,
        },
        h264::{H264SampleEntryConfig, LengthSize, build_avc1_box, build_avc1_box_from_annexb},
        h265::{H265ConstantFrameRate, H265SampleEntryConfig, build_hvc1_box_from_annexb},
    },
    boxes::SampleEntry,
    codec_string,
    descriptors::EsDescriptor,
    mux::Sample,
};

use super::rendition::BufferedSample;
use restream_core::codec::adts_frame_count;
use restream_core::codec::avcc_record;
use restream_core::codec::build_aac_sequence_header;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, PayloadFormat};
use restream_rtmp::enhanced_rtmp_hevc::EX_CODED_FRAMES;
use restream_rtmp::enhanced_rtmp_hevc::EX_CODED_FRAMES_X;
use restream_rtmp::enhanced_rtmp_hevc::EX_SEQUENCE_START;
use restream_rtmp::enhanced_rtmp_hevc::ex_video_tag;
use restream_rtmp::flv::flv_video_config;

pub(super) const VIDEO_TIMESCALE: u32 = 90_000;

pub(super) fn build_mux_samples(
    buffered: &[BufferedSample],
    track_kind: TrackKind,
    timescale: u32,
    sample_entry: SampleEntry,
    next_segment_first_dts: Option<i64>,
) -> Result<Vec<Sample>, String> {
    let timescale = NonZeroU32::new(timescale).ok_or_else(|| "zero timescale".to_string())?;
    let mut samples = Vec::with_capacity(buffered.len());
    let mut buffered_iter = buffered.iter().peekable();
    while let Some(sample) = buffered_iter.next() {
        let next_dts = buffered_iter
            .peek()
            .map(|next| next.dts)
            .or(next_segment_first_dts)
            .unwrap_or_else(|| {
                sample
                    .dts
                    .saturating_add(i64::from(sample.default_duration))
            });
        let duration = next_dts.saturating_sub(sample.dts);
        if duration <= 0 || duration > u32::MAX as i64 {
            return Err(format!("invalid sample duration: {duration}"));
        }
        let composition_time_offset = if track_kind == TrackKind::Video {
            let cto = sample.pts.saturating_sub(sample.dts);
            if cto == 0 {
                None
            } else if !(i32::MIN as i64..=i32::MAX as i64).contains(&cto) {
                return Err(format!("composition offset out of i32 range: {cto}"));
            } else {
                Some(cto)
            }
        } else {
            None
        };
        samples.push(Sample {
            track_kind,
            sample_entry: Some(sample_entry.clone()),
            keyframe: sample.keyframe,
            timescale,
            duration: duration as u32,
            composition_time_offset,
            data_offset: sample.data_offset,
            data_size: sample.data_size,
        });
    }
    Ok(samples)
}

/// The video codec a preview rendition packages: H.264 as `avc1`, H.265 as
/// `hvc1` (parameter sets out of band, in the sample entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PreviewVideoCodec {
    H264,
    H265,
}

impl PreviewVideoCodec {
    pub(super) fn of(video: &VideoMeta) -> Self {
        if video.codec.eq_ignore_ascii_case("hevc") || video.codec.eq_ignore_ascii_case("h265") {
            Self::H265
        } else {
            Self::H264
        }
    }
}

/// The codec of an FLV video sequence header of any kind the preview reads:
/// classic H.264, legacy H.265 (codec id 12), or an enhanced RTMP sequence
/// start; `None` for any other tag.
pub(super) fn flv_sequence_header_codec(payload: &[u8]) -> Option<PreviewVideoCodec> {
    if let Some(tag) = ex_video_tag(payload) {
        return (tag.packet_type == EX_SEQUENCE_START).then_some(if tag.is_hevc() {
            PreviewVideoCodec::H265
        } else {
            PreviewVideoCodec::H264
        });
    }
    match payload {
        [tag, 0, ..] if tag & 0x0F == 7 => Some(PreviewVideoCodec::H264),
        [tag, 0, ..] if tag & 0x0F == 12 => Some(PreviewVideoCodec::H265),
        _ => None,
    }
}

pub(super) fn is_flv_video_sequence_header(payload: &[u8]) -> bool {
    flv_sequence_header_codec(payload).is_some()
}

/// The sample entry an FLV video sequence header describes: `avc1` for
/// H.264, `hvc1` for H.265.
pub(super) fn build_sample_entry_from_flv_sequence_header(header: &[u8]) -> Option<SampleEntry> {
    if is_flv_avc_sequence_header(header) {
        return build_h264_sample_entry_from_flv_sequence_header(header);
    }
    let hevc = flv_sequence_header_codec(header) == Some(PreviewVideoCodec::H265);
    let (nalu_len_size, annexb) = flv_video_config(header)?;
    let length_size =
        LengthSize::from_length_size_minus_one(u8::try_from(nalu_len_size.checked_sub(1)?).ok()?)
            .ok()?;
    if hevc {
        build_hevc_sample_entry(&annexb, length_size)
    } else {
        build_avc1_box_from_annexb(&annexb, &H264SampleEntryConfig { length_size })
            .ok()
            .map(SampleEntry::Avc1)
    }
}

fn build_hevc_sample_entry(annexb: &[u8], length_size: LengthSize) -> Option<SampleEntry> {
    build_hvc1_box_from_annexb(
        annexb,
        &H265SampleEntryConfig {
            length_size,
            avg_frame_rate: H265SampleEntryConfig::AVG_FRAME_RATE_UNSPECIFIED,
            constant_frame_rate: H265ConstantFrameRate::Unknown,
        },
    )
    .ok()
    .map(SampleEntry::Hvc1)
}

/// The sample entry a video packet carries, if it carries one: an FLV
/// sequence header, or a raw keyframe with its parameter sets in band.
pub(super) fn build_sample_entry_from_video_packet(
    packet: &MediaPacket,
    codec: PreviewVideoCodec,
) -> Option<SampleEntry> {
    match (packet.format, codec) {
        (PayloadFormat::Flv, _) => (flv_sequence_header_codec(&packet.payload) == Some(codec))
            .then(|| build_sample_entry_from_flv_sequence_header(&packet.payload))
            .flatten(),
        (PayloadFormat::Raw, PreviewVideoCodec::H264) => {
            build_h264_sample_entry_from_video_packet(packet)
        }
        (PayloadFormat::Raw, PreviewVideoCodec::H265) => {
            build_hevc_sample_entry(&packet.payload, LengthSize::FourBytes)
        }
    }
}

/// The length-prefixed NAL units of an FLV coded-frame tag (classic or
/// enhanced); `None` for a sequence header or any other tag.
pub(super) fn flv_coded_frame_body(payload: &[u8]) -> Option<&[u8]> {
    if let Some(tag) = ex_video_tag(payload) {
        return matches!(tag.packet_type, EX_CODED_FRAMES | EX_CODED_FRAMES_X)
            .then_some(tag.body)
            .filter(|body| !body.is_empty());
    }
    match payload {
        [_, packet_type, ..] if *packet_type != 0 => {
            payload.get(5..).filter(|body| !body.is_empty())
        }
        _ => None,
    }
}

pub(super) fn is_flv_avc_sequence_header(payload: &[u8]) -> bool {
    matches!(payload, [tag, 0, ..] if tag & 0x0F == 7)
}

pub(super) fn build_h264_sample_entry_from_video_packet(
    packet: &MediaPacket,
) -> Option<SampleEntry> {
    match packet.format {
        PayloadFormat::Flv => {
            if is_flv_avc_sequence_header(&packet.payload) {
                build_h264_sample_entry_from_flv_sequence_header(&packet.payload)
            } else {
                None
            }
        }
        PayloadFormat::Raw => build_avc1_box_from_annexb(
            &packet.payload,
            &H264SampleEntryConfig {
                length_size: LengthSize::FourBytes,
            },
        )
        .ok()
        .map(SampleEntry::Avc1),
    }
}

pub(super) fn build_h264_sample_entry_from_flv_sequence_header(
    sequence_header: &[u8],
) -> Option<SampleEntry> {
    if !is_flv_avc_sequence_header(sequence_header) {
        return None;
    }
    let record = avcc_record(sequence_header.get(5..)?)?;
    let length_size = LengthSize::from_length_size_minus_one(record.length_size_minus_one).ok()?;
    let owned = |list: &[&[u8]]| list.iter().map(|nalu| nalu.to_vec()).collect::<Vec<_>>();
    build_avc1_box(
        &owned(&record.sps),
        &owned(&record.pps),
        &H264SampleEntryConfig { length_size },
    )
    .ok()
    .map(SampleEntry::Avc1)
}

pub(super) fn sample_entry_codec_string(sample_entry: &SampleEntry) -> Option<String> {
    codec_string::from_sample_entry(sample_entry).ok()
}

pub(super) fn build_aac_sample_entry(
    track: &AudioMeta,
    audio_sequence_header: Option<&[u8]>,
) -> Option<SampleEntry> {
    let asc = aac_specific_config(track, audio_sequence_header)?;
    build_mp4a_box(
        &asc,
        &Mp4aSampleEntryConfig {
            es_id: EsDescriptor::MIN_ES_ID,
            buffer_size_db: 0,
            max_bitrate: 0,
            avg_bitrate: 0,
        },
    )
    .ok()
    .map(SampleEntry::Mp4a)
}

fn aac_specific_config(track: &AudioMeta, header: Option<&[u8]>) -> Option<AudioSpecificConfig> {
    header
        .and_then(parse_flv_audio_specific_config)
        .or_else(|| parse_generated_asc(track.sample_rate, track.channels))
        .or_else(default_aac_lc_stereo_48k)
}

fn parse_flv_audio_specific_config(bytes: &[u8]) -> Option<AudioSpecificConfig> {
    let payload = bytes.get(2..).filter(|payload| !payload.is_empty())?;
    parse_audio_specific_config(payload).ok().or_else(|| {
        payload
            .get(..2)
            .and_then(|short| parse_audio_specific_config(short).ok())
    })
}

fn parse_generated_asc(sample_rate: u32, channels: u32) -> Option<AudioSpecificConfig> {
    let header = build_aac_sequence_header(sample_rate, channels);
    parse_flv_audio_specific_config(header.as_ref())
}

fn default_aac_lc_stereo_48k() -> Option<AudioSpecificConfig> {
    Some(AudioSpecificConfig {
        audio_object_type: AudioObjectType::AacLc,
        sampling_frequency: SamplingFrequency::from_hz(48_000).ok()?,
        channel_configuration: ChannelConfiguration::Stereo,
    })
}

pub(super) fn default_video_duration(video: &VideoMeta) -> u32 {
    if video.fps.is_finite() && video.fps > 0.0 {
        ((VIDEO_TIMESCALE as f64 / video.fps).round() as u32).max(1)
    } else {
        3_000
    }
}

pub(super) fn audio_default_duration(packet: &MediaPacket, sample_rate: u32) -> u32 {
    let frames = match packet.format {
        PayloadFormat::Flv => 1,
        PayloadFormat::Raw => {
            let count = adts_frame_count(&packet.payload);
            if count == 0 { 1 } else { count }
        }
    };
    let frame_samples = 1024u32;
    let duration = frame_samples.saturating_mul(frames as u32);
    duration.min(sample_rate.max(duration)).max(1)
}

pub(super) fn rescale_ms(ms: i64, timescale: u32) -> i64 {
    ms.saturating_mul(timescale as i64) / 1000
}
