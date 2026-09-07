use std::num::NonZeroU32;

use shiguredo_mp4::{
    TrackKind,
    bitstream::{
        aac::{
            AudioObjectType, AudioSpecificConfig, ChannelConfiguration, Mp4aSampleEntryConfig,
            SamplingFrequency, build_mp4a_box, parse_audio_specific_config,
        },
        h264::{H264SampleEntryConfig, LengthSize, build_avc1_box, build_avc1_box_from_annexb},
    },
    boxes::SampleEntry,
    codec_string,
    descriptors::EsDescriptor,
    mux::Sample,
};

use super::rendition::BufferedSample;
use crate::media::codec::{adts_frame_count, build_aac_sequence_header};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, PayloadFormat};

pub(super) const VIDEO_TIMESCALE: u32 = 90_000;

pub(super) struct AvccNalLists {
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub length_size: u8,
}

pub(super) fn build_mux_samples(
    buffered: &[BufferedSample],
    track_kind: TrackKind,
    timescale: u32,
    sample_entry: SampleEntry,
    next_segment_first_dts: Option<i64>,
) -> Result<Vec<Sample>, String> {
    let timescale = NonZeroU32::new(timescale).ok_or_else(|| "zero timescale".to_string())?;
    let mut samples = Vec::with_capacity(buffered.len());
    for (index, sample) in buffered.iter().enumerate() {
        let next_dts = buffered
            .get(index + 1)
            .map(|next| next.dts)
            .or(next_segment_first_dts)
            .unwrap_or_else(|| sample.dts + sample.default_duration as i64);
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

pub(super) fn is_flv_avc_sequence_header(payload: &[u8]) -> bool {
    payload.len() > 1 && (payload[0] & 0x0F) == 7 && payload[1] == 0
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
    let avcc = sequence_header.get(5..)?;
    let lists = parse_avcc_nal_lists(avcc)?;
    let length_size = LengthSize::from_length_size_minus_one(lists.length_size).ok()?;
    build_avc1_box(
        &lists.sps,
        &lists.pps,
        &H264SampleEntryConfig { length_size },
    )
    .ok()
    .map(SampleEntry::Avc1)
}

pub(super) fn parse_avcc_nal_lists(data: &[u8]) -> Option<AvccNalLists> {
    if data.len() < 7 {
        return None;
    }
    let mut pos = 6usize;
    let num_sps = (data[5] & 0x1F) as usize;
    let mut sps = Vec::with_capacity(num_sps);
    for _ in 0..num_sps {
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2;
        sps.push(data.get(pos..pos + len)?.to_vec());
        pos += len;
    }
    let num_pps = *data.get(pos)? as usize;
    pos += 1;
    let mut pps = Vec::with_capacity(num_pps);
    for _ in 0..num_pps {
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2;
        pps.push(data.get(pos..pos + len)?.to_vec());
        pos += len;
    }
    Some(AvccNalLists {
        sps,
        pps,
        length_size: data[4] & 0x03,
    })
}

pub(super) fn sample_entry_codec_string(sample_entry: &SampleEntry) -> Option<String> {
    codec_string::from_sample_entry(sample_entry).ok()
}

pub(super) fn build_aac_sample_entry(
    track: &AudioMeta,
    audio_sequence_header: Option<&[u8]>,
) -> Option<SampleEntry> {
    let asc = aac_specific_config(track, audio_sequence_header);
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

fn aac_specific_config(track: &AudioMeta, header: Option<&[u8]>) -> AudioSpecificConfig {
    header
        .and_then(parse_flv_audio_specific_config)
        .or_else(|| parse_generated_asc(track.sample_rate, track.channels))
        .unwrap_or_else(default_aac_lc_stereo_48k)
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

fn default_aac_lc_stereo_48k() -> AudioSpecificConfig {
    AudioSpecificConfig {
        audio_object_type: AudioObjectType::AacLc,
        sampling_frequency: SamplingFrequency::from_hz(48_000)
            .expect("48 kHz is an AAC table rate"),
        channel_configuration: ChannelConfiguration::Stereo,
    }
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
