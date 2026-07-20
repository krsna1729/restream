use std::num::NonZeroU32;

use shiguredo_mp4::{
    FixedPointNumber, TrackKind, Uint,
    boxes::EsdsBox,
    boxes::{
        AudioSampleEntryFields, Avc1Box, AvccBox, Mp4aBox, SampleEntry, VisualSampleEntryFields,
    },
    descriptors::{DecoderConfigDescriptor, DecoderSpecificInfo, EsDescriptor, SlConfigDescriptor},
    mux::Sample,
};

use super::rendition::BufferedSample;
use crate::media::codec::{
    adts_frame_count, build_aac_sequence_header, build_avcc_sequence_header,
};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, PayloadFormat};

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

pub(super) fn build_h264_sample_entry_from_video_packet(
    packet: &MediaPacket,
    video: &VideoMeta,
) -> Option<SampleEntry> {
    match packet.format {
        PayloadFormat::Flv => {
            build_h264_sample_entry_from_flv_sequence_header(&packet.payload, video)
        }
        PayloadFormat::Raw => {
            let seq = build_avcc_sequence_header(&packet.payload)?;
            build_h264_sample_entry_from_flv_sequence_header(seq.as_ref(), video)
        }
    }
}

pub(super) fn build_h264_sample_entry_from_flv_sequence_header(
    sequence_header: &[u8],
    video: &VideoMeta,
) -> Option<SampleEntry> {
    let avcc = sequence_header.get(5..)?;
    let avcc_box = parse_avcc_box(avcc)?;
    Some(SampleEntry::Avc1(Avc1Box {
        visual: VisualSampleEntryFields {
            data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            width: video.width.min(u16::MAX as u32) as u16,
            height: video.height.min(u16::MAX as u32) as u16,
            horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
            vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
            frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
            compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
            depth: VisualSampleEntryFields::DEFAULT_DEPTH,
        },
        avcc_box,
        unknown_boxes: Vec::new(),
    }))
}

pub(super) fn parse_avcc_box(data: &[u8]) -> Option<AvccBox> {
    if data.len() < 7 {
        return None;
    }
    let mut pos = 6usize;
    let num_sps = (data[5] & 0x1F) as usize;
    let mut sps_list = Vec::with_capacity(num_sps);
    for _ in 0..num_sps {
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2;
        sps_list.push(data.get(pos..pos + len)?.to_vec());
        pos += len;
    }
    let num_pps = *data.get(pos)? as usize;
    pos += 1;
    let mut pps_list = Vec::with_capacity(num_pps);
    for _ in 0..num_pps {
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2;
        pps_list.push(data.get(pos..pos + len)?.to_vec());
        pos += len;
    }
    let mut avcc = AvccBox {
        avc_profile_indication: data[1],
        profile_compatibility: data[2],
        avc_level_indication: data[3],
        length_size_minus_one: Uint::new(data[4] & 0x03),
        sps_list,
        pps_list,
        chroma_format: None,
        bit_depth_luma_minus8: None,
        bit_depth_chroma_minus8: None,
        sps_ext_list: Vec::new(),
    };
    if let Some(sps) = avcc.sps_list.first()
        && let Some(fields) = parse_h264_sps_avcc_fields(sps)
    {
        avcc.chroma_format = Some(fields.chroma_format);
        avcc.bit_depth_luma_minus8 = Some(fields.bit_depth_luma_minus8);
        avcc.bit_depth_chroma_minus8 = Some(fields.bit_depth_chroma_minus8);
    }
    Some(avcc)
}

struct AvccProfileFields {
    chroma_format: Uint<u8, 2>,
    bit_depth_luma_minus8: Uint<u8, 3>,
    bit_depth_chroma_minus8: Uint<u8, 3>,
}

fn parse_h264_sps_avcc_fields(sps_nalu: &[u8]) -> Option<AvccProfileFields> {
    if sps_nalu.is_empty() {
        return None;
    }
    let rbsp = remove_emulation_prevention_bytes(sps_nalu);
    let mut reader = H264BitReader::new(&rbsp);
    reader.skip(8)?;
    let profile_idc = reader.read_bits(8)? as u8;
    reader.skip(8)?;
    reader.skip(8)?;
    let _seq_parameter_set_id = reader.read_exp_golomb()?;

    if matches!(profile_idc, 66 | 77 | 88) {
        return None;
    }

    let chroma_format = reader.read_exp_golomb()?;
    if chroma_format > 3 {
        return None;
    }
    if chroma_format == 3 {
        reader.skip(1)?;
    }

    let bit_depth_luma_minus8 = reader.read_exp_golomb()?;
    let bit_depth_chroma_minus8 = reader.read_exp_golomb()?;
    if bit_depth_luma_minus8 > 7 || bit_depth_chroma_minus8 > 7 {
        return None;
    }

    Some(AvccProfileFields {
        chroma_format: Uint::new(chroma_format as u8),
        bit_depth_luma_minus8: Uint::new(bit_depth_luma_minus8 as u8),
        bit_depth_chroma_minus8: Uint::new(bit_depth_chroma_minus8 as u8),
    })
}

fn remove_emulation_prevention_bytes(data: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if index + 2 < data.len()
            && data[index] == 0
            && data[index + 1] == 0
            && data[index + 2] == 3
        {
            rbsp.push(0);
            rbsp.push(0);
            index += 3;
        } else {
            rbsp.push(data[index]);
            index += 1;
        }
    }
    rbsp
}

struct H264BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> H264BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bits(&mut self, count: usize) -> Option<u32> {
        if count == 0 {
            return Some(0);
        }
        let mut value = 0u32;
        for _ in 0..count {
            let byte = *self.data.get(self.bit_pos / 8)?;
            let shift = 7 - (self.bit_pos % 8);
            value = (value << 1) | ((byte >> shift) & 1) as u32;
            self.bit_pos += 1;
        }
        Some(value)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.read_bits(count).map(|_| ())
    }

    fn read_exp_golomb(&mut self) -> Option<u32> {
        let mut leading_zero_bits = 0usize;
        while self.read_bits(1)? == 0 {
            leading_zero_bits += 1;
            // ue(v) codes wider than 32 bits cannot occur in any field this
            // reader parses; a run this long only happens on malformed input
            // (or past the end of the buffer) and would otherwise overflow
            // the `1u32 << leading_zero_bits` below. Fail closed instead.
            if leading_zero_bits >= 32 {
                return None;
            }
        }
        let suffix = if leading_zero_bits == 0 {
            0
        } else {
            self.read_bits(leading_zero_bits)?
        };
        Some(((1u32 << leading_zero_bits) - 1) + suffix)
    }
}

pub(super) fn sample_entry_to_avcc_bytes(sample_entry: &SampleEntry) -> Option<Vec<u8>> {
    match sample_entry {
        SampleEntry::Avc1(avc1) => {
            let avcc = &avc1.avcc_box;
            let mut bytes = Vec::with_capacity(64);
            bytes.push(1);
            bytes.push(avcc.avc_profile_indication);
            bytes.push(avcc.profile_compatibility);
            bytes.push(avcc.avc_level_indication);
            bytes.push(0xFC | avcc.length_size_minus_one.get());
            bytes.push(0xE0 | avcc.sps_list.len() as u8);
            for sps in &avcc.sps_list {
                bytes.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                bytes.extend_from_slice(sps);
            }
            bytes.push(avcc.pps_list.len() as u8);
            for pps in &avcc.pps_list {
                bytes.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                bytes.extend_from_slice(pps);
            }
            Some(bytes)
        }
        _ => None,
    }
}

pub(super) fn build_aac_sample_entry(
    track: &AudioMeta,
    audio_sequence_header: Option<&[u8]>,
) -> SampleEntry {
    let asc = audio_sequence_header
        .filter(|bytes| bytes.len() >= 4)
        .map(|bytes| bytes[2..4].to_vec())
        .unwrap_or_else(|| {
            build_aac_sequence_header(track.sample_rate, track.channels)
                .slice(2..4)
                .to_vec()
        });
    SampleEntry::Mp4a(Mp4aBox {
        audio: AudioSampleEntryFields {
            data_reference_index: AudioSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            channelcount: track.channels.min(u16::MAX as u32) as u16,
            samplesize: AudioSampleEntryFields::DEFAULT_SAMPLESIZE,
            samplerate: FixedPointNumber::new(track.sample_rate.min(u16::MAX as u32) as u16, 0),
        },
        esds_box: EsdsBox {
            es: EsDescriptor {
                es_id: EsDescriptor::MIN_ES_ID,
                stream_priority: EsDescriptor::LOWEST_STREAM_PRIORITY,
                depends_on_es_id: None,
                url_string: None,
                ocr_es_id: None,
                dec_config_descr: DecoderConfigDescriptor {
                    object_type_indication:
                        DecoderConfigDescriptor::OBJECT_TYPE_INDICATION_AUDIO_ISO_IEC_14496_3,
                    stream_type: DecoderConfigDescriptor::STREAM_TYPE_AUDIO,
                    up_stream: DecoderConfigDescriptor::UP_STREAM_FALSE,
                    buffer_size_db: Uint::new(0),
                    max_bitrate: 0,
                    avg_bitrate: 0,
                    dec_specific_info: Some(DecoderSpecificInfo { payload: asc }),
                },
                sl_config_descr: SlConfigDescriptor,
            },
        },
        unknown_boxes: Vec::new(),
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
