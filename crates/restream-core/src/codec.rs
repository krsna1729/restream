//! Core codec descriptors and Annex B / ADTS utilities.

use bytes::Bytes;
use std::ops::ControlFlow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodecKind {
    H264,
    Hevc,
    Unknown,
}

impl VideoCodecKind {
    pub fn from_codec_name(codec: &str) -> Self {
        if codec.eq_ignore_ascii_case("h264") || codec.eq_ignore_ascii_case("avc") {
            Self::H264
        } else if codec.eq_ignore_ascii_case("h265") || codec.eq_ignore_ascii_case("hevc") {
            Self::Hevc
        } else {
            Self::Unknown
        }
    }

    pub fn is_hevc(self) -> bool {
        matches!(self, Self::Hevc)
    }

    pub const fn as_stage_codec(self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("h264"),
            Self::Hevc => Some("hevc"),
            Self::Unknown => None,
        }
    }
}

fn get_start_code_finder() -> &'static memchr::memmem::Finder<'static> {
    static FINDER: std::sync::OnceLock<memchr::memmem::Finder<'static>> =
        std::sync::OnceLock::new();
    FINDER.get_or_init(|| memchr::memmem::Finder::new(&[0u8, 0, 1]))
}

fn start_code_span(data: &[u8], idx: usize) -> (usize, usize) {
    let zeros = data.get(..idx).map_or(0, |before| {
        before.iter().rev().take_while(|&&byte| byte == 0).count()
    });
    (idx.saturating_sub(zeros), idx.saturating_add(3))
}

pub fn find_annexb_start_codes(data: &[u8]) -> Vec<(usize, usize)> {
    let finder = get_start_code_finder();
    finder
        .find_iter(data)
        .map(|idx| start_code_span(data, idx))
        .collect()
}

pub fn split_annexb_nalus(data: &[u8]) -> Vec<&[u8]> {
    let mut nalus = Vec::new();
    let _ = for_each_annexb_nalu(data, |nalu| {
        nalus.push(nalu);
        ControlFlow::Continue(())
    });
    nalus
}

pub fn for_each_annexb_nalu<'a>(
    data: &'a [u8],
    mut visit: impl FnMut(&'a [u8]) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let finder = get_start_code_finder();
    let mut current: Option<usize> = None;
    for idx in finder.find_iter(data) {
        let (start, payload_start) = start_code_span(data, idx);
        if let Some(nalu_start) = current
            && let Some(nalu) = data.get(nalu_start..start)
            && !nalu.is_empty()
        {
            visit(nalu)?;
        }
        current = Some(payload_start);
    }
    if let Some(nalu_start) = current
        && let Some(nalu) = data.get(nalu_start..)
        && !nalu.is_empty()
    {
        visit(nalu)?;
    }
    ControlFlow::Continue(())
}

fn annexb_nalu(nalu: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + nalu.len());
    out.extend_from_slice(&[0, 0, 0, 1]);
    out.extend_from_slice(nalu);
    out
}

pub fn raw_annexb_is_keyframe(payload: &[u8]) -> bool {
    for_each_annexb_nalu(payload, |nalu| {
        let keyframe = match nalu {
            [first, ..] if first & 0x1F == 5 => true,
            [first, _, ..] => matches!((first >> 1) & 0x3F, 16..=23),
            _ => false,
        };
        if keyframe {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .is_break()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnnexbCodecKind {
    #[default]
    Unknown,
    H264,
    H265,
}

#[derive(Default)]
pub struct AnnexbParameterSetAccumulator {
    kind: AnnexbCodecKind,
    h264_sps: Option<Vec<u8>>,
    h264_pps: Option<Vec<u8>>,
    h265_vps: Option<Vec<u8>>,
    h265_sps: Option<Vec<u8>>,
    h265_pps: Option<Vec<u8>>,
}

impl AnnexbParameterSetAccumulator {
    pub fn push_payload(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        let _ = for_each_annexb_nalu(payload, |nalu| {
            self.push_nalu(nalu);
            ControlFlow::Continue(())
        });
        self.complete()
    }

    fn push_nalu(&mut self, nalu: &[u8]) {
        let Some(&first) = nalu.first() else {
            return;
        };
        let h264_nal_type = first & 0x1F;
        let h265_nal_type = match nalu {
            [first, second, ..] if first & 0x81 == 0 && (1..=7).contains(second) => {
                (first >> 1) & 0x3F
            }
            _ => 0,
        };

        if (32..=34).contains(&h265_nal_type) && self.switch_kind(AnnexbCodecKind::H265) {
            match h265_nal_type {
                32 => self.h265_vps = Some(annexb_nalu(nalu)),
                33 => self.h265_sps = Some(annexb_nalu(nalu)),
                34 => self.h265_pps = Some(annexb_nalu(nalu)),
                _ => {}
            }
            return;
        }

        if self.kind != AnnexbCodecKind::H265
            && matches!(h264_nal_type, 7 | 8)
            && self.switch_kind(AnnexbCodecKind::H264)
        {
            if h264_nal_type == 7 {
                self.h264_sps = Some(annexb_nalu(nalu));
            } else {
                self.h264_pps = Some(annexb_nalu(nalu));
            }
        }
    }

    fn switch_kind(&mut self, kind: AnnexbCodecKind) -> bool {
        match self.kind {
            AnnexbCodecKind::Unknown => {
                self.kind = kind;
                true
            }
            existing if existing == kind => true,
            _ => {
                *self = Self {
                    kind,
                    ..Self::default()
                };
                true
            }
        }
    }

    fn complete(&self) -> Option<Vec<u8>> {
        match self.kind {
            AnnexbCodecKind::Unknown => None,
            AnnexbCodecKind::H264 => {
                let (Some(sps), Some(pps)) = (&self.h264_sps, &self.h264_pps) else {
                    return None;
                };
                Some([sps.as_slice(), pps.as_slice()].concat())
            }
            AnnexbCodecKind::H265 => {
                let (Some(vps), Some(sps), Some(pps)) =
                    (&self.h265_vps, &self.h265_sps, &self.h265_pps)
                else {
                    return None;
                };
                Some([vps.as_slice(), sps.as_slice(), pps.as_slice()].concat())
            }
        }
    }
}

pub fn annexb_parameter_sets(payload: &[u8]) -> Option<Vec<u8>> {
    let mut accumulator = AnnexbParameterSetAccumulator::default();
    accumulator.push_payload(payload)
}

fn write_avcc_record_into(annexb_data: &[u8], body: &mut Vec<u8>) -> bool {
    let nalus = split_annexb_nalus(annexb_data);
    let nal_type = |nalu: &&[u8]| nalu.first().map(|first| first & 0x1F);
    let sps_list: Vec<&[u8]> = nalus
        .iter()
        .filter(|nalu| nal_type(nalu) == Some(7))
        .copied()
        .collect();
    let pps_list: Vec<&[u8]> = nalus
        .iter()
        .filter(|nalu| nal_type(nalu) == Some(8))
        .copied()
        .collect();

    let Some(&[_, profile, compatibility, level]) =
        sps_list.first().and_then(|s| s.first_chunk::<4>())
    else {
        return false;
    };
    let Some(sps_count) = u8::try_from(sps_list.len())
        .ok()
        .filter(|count| *count <= 0x1F)
    else {
        return false;
    };
    let Ok(pps_count) = u8::try_from(pps_list.len()) else {
        return false;
    };
    let too_long = |nalu: &&[u8]| u16::try_from(nalu.len()).is_err();
    if sps_list.is_empty()
        || pps_list.is_empty()
        || sps_list.iter().any(too_long)
        || pps_list.iter().any(too_long)
    {
        return false;
    }
    body.push(1); // configurationVersion
    body.push(profile);
    body.push(compatibility);
    body.push(level);
    body.push(0xFF); // lengthSizeMinusOne = 3 (4 bytes)
    body.push(0xE0 | sps_count);
    for sps in sps_list {
        body.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        body.extend_from_slice(sps);
    }
    body.push(pps_count);
    for pps in pps_list {
        body.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        body.extend_from_slice(pps);
    }
    true
}

pub fn build_avcc_record(annexb_data: &[u8]) -> Option<Bytes> {
    let mut body = Vec::with_capacity(64);
    if write_avcc_record_into(annexb_data, &mut body) {
        Some(Bytes::from(body))
    } else {
        None
    }
}

pub fn build_avcc_sequence_header(annexb_data: &[u8]) -> Option<Bytes> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&[0x17, 0x00, 0x00, 0x00, 0x00]);
    if write_avcc_record_into(annexb_data, &mut out) {
        Some(Bytes::from(out))
    } else {
        None
    }
}

pub const AAC_SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

pub fn adts_frame_count(data: &[u8]) -> usize {
    adts_frame_ranges(data).count()
}

pub fn adts_frame_ranges(data: &[u8]) -> AdtsFrameRanges<'_> {
    AdtsFrameRanges { data, offset: 0 }
}

pub struct AdtsFrameRanges<'a> {
    data: &'a [u8],
    offset: usize,
}

impl Iterator for AdtsFrameRanges<'_> {
    type Item = std::ops::Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.data.get(self.offset..)?;
        let Some(&[0xFF, sync_low, _, b3, b4, b5, _]) = rest.first_chunk::<7>() else {
            return None;
        };
        if sync_low & 0xF0 != 0xF0 {
            return None;
        }
        let frame_len =
            (usize::from(b3 & 0x03) << 11) | (usize::from(b4) << 3) | (usize::from(b5 & 0xE0) >> 5);
        if frame_len < 7 || rest.len() < frame_len {
            return None;
        }
        let start = self.offset;
        self.offset = self.offset.saturating_add(frame_len);
        Some(start..self.offset)
    }
}

pub fn adts_sample_rate(data: &[u8]) -> Option<u32> {
    let header = data.first_chunk::<3>()?;
    if header[0] != 0xFF || header[1] & 0xF0 != 0xF0 {
        return None;
    }
    AAC_SAMPLE_RATES
        .get(usize::from((header[2] >> 2) & 0x0F))
        .copied()
}

/// Most-significant-bit-first reader over an RBSP.
pub struct BitReader<'a> {
    data: &'a [u8],
    mask: u8,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, mask: 0x80 }
    }

    fn read_bit(&mut self) -> Option<bool> {
        let (&byte, rest) = self.data.split_first()?;
        let bit = byte & self.mask != 0;
        if self.mask == 0x01 {
            self.mask = 0x80;
            self.data = rest;
        } else {
            self.mask /= 2;
        }
        Some(bit)
    }

    pub fn read_bits(&mut self, n: u32) -> Option<u32> {
        if n > 32 {
            return None;
        }
        let mut value = 0u32;
        for _ in 0..n {
            value = value.checked_mul(2)? | u32::from(self.read_bit()?);
        }
        Some(value)
    }

    pub fn skip(&mut self, n: u32) -> Option<()> {
        for _ in 0..n {
            self.read_bit()?;
        }
        Some(())
    }

    pub fn read_ue(&mut self) -> Option<u32> {
        let mut leading_zeros = 0u32;
        while !self.read_bit()? {
            leading_zeros = leading_zeros.checked_add(1)?;
            if leading_zeros > 31 {
                return None;
            }
        }
        if leading_zeros == 0 {
            return Some(0);
        }
        let suffix = self.read_bits(leading_zeros)?;
        1u32.checked_shl(leading_zeros)?
            .checked_sub(1)?
            .checked_add(suffix)
    }

    pub fn read_se(&mut self) -> Option<i32> {
        let code = i64::from(self.read_ue()?);
        let magnitude = code.checked_add(1)? / 2;
        let value = if code & 1 == 1 {
            magnitude
        } else {
            magnitude.checked_neg()?
        };
        i32::try_from(value).ok()
    }
}

pub fn rbsp(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0u8;
    for &byte in data {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        out.push(byte);
        zeros = if byte == 0 {
            zeros.saturating_add(1)
        } else {
            0
        };
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct H264Sps {
    pub profile_idc: u8,
    pub level_idc: u8,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

const HIGH_PROFILES: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

pub fn profile_name(profile_idc: u8) -> &'static str {
    match profile_idc {
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 => "High 10",
        122 => "High 4:2:2",
        244 => "High 4:4:4 Predictive",
        _ => "Unknown",
    }
}

pub fn level_name(level_idc: u8) -> String {
    format!("{}.{}", level_idc / 10, level_idc % 10)
}

pub fn parse_h264_sps(payload: &[u8]) -> Option<H264Sps> {
    let sps = rbsp(payload);
    let &[profile_idc, _constraints, level_idc] = sps.first_chunk::<3>()?;
    let mut reader = BitReader::new(sps.get(3..)?);
    let _seq_parameter_set_id = reader.read_ue()?;

    let chroma_format_idc = if HIGH_PROFILES.contains(&profile_idc) {
        let chroma_format_idc = reader.read_ue()?;
        if chroma_format_idc > 3 {
            return None;
        }
        if chroma_format_idc == 3 {
            reader.skip(1)?;
        }
        let bit_depth_luma_minus8 = reader.read_ue()?;
        let bit_depth_chroma_minus8 = reader.read_ue()?;
        if bit_depth_luma_minus8 > 8 || bit_depth_chroma_minus8 > 8 {
            return None;
        }
        reader.skip(1)?;
        if reader.read_bits(1)? == 1 {
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for index in 0..lists {
                if reader.read_bits(1)? == 1 {
                    skip_scaling_list(&mut reader, if index < 6 { 16 } else { 64 })?;
                }
            }
        }
        chroma_format_idc
    } else {
        1
    };

    let _log2_max_frame_num_minus4 = reader.read_ue()?;
    match reader.read_ue()? {
        0 => {
            let _log2_max_pic_order_cnt_lsb_minus4 = reader.read_ue()?;
        }
        1 => {
            reader.skip(1)?;
            reader.read_se()?;
            reader.read_se()?;
            let cycle = reader.read_ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                reader.read_se()?;
            }
        }
        2 => {}
        _ => return None,
    }
    let _max_num_ref_frames = reader.read_ue()?;
    reader.skip(1)?;
    let width_in_mbs_minus1 = reader.read_ue()?;
    let height_in_map_units_minus1 = reader.read_ue()?;
    let frame_mbs_only = reader.read_bits(1)?;
    if frame_mbs_only == 0 {
        reader.skip(1)?;
    }
    reader.skip(1)?;
    let (crop_left, crop_right, crop_top, crop_bottom) = if reader.read_bits(1)? == 1 {
        (
            reader.read_ue()?,
            reader.read_ue()?,
            reader.read_ue()?,
            reader.read_ue()?,
        )
    } else {
        (0, 0, 0, 0)
    };

    let sub_width = if matches!(chroma_format_idc, 1 | 2) {
        2
    } else {
        1
    };
    let sub_height: u64 = if chroma_format_idc == 1 { 2 } else { 1 };
    let frame_height_factor = 2u64.checked_sub(u64::from(frame_mbs_only))?;
    let width = cropped(
        u64::from(width_in_mbs_minus1)
            .checked_add(1)?
            .checked_mul(16)?,
        crop_left,
        crop_right,
        sub_width,
    )?;
    let height = cropped(
        u64::from(height_in_map_units_minus1)
            .checked_add(1)?
            .checked_mul(frame_height_factor)?
            .checked_mul(16)?,
        crop_top,
        crop_bottom,
        sub_height.checked_mul(frame_height_factor)?,
    )?;

    Some(H264Sps {
        profile_idc,
        level_idc,
        width,
        height,
        fps: if reader.read_bits(1)? == 1 {
            vui_frame_rate(&mut reader)?
        } else {
            0.0
        },
    })
}

fn cropped(size: u64, first: u32, second: u32, unit: u64) -> Option<u32> {
    let crop = u64::from(first)
        .checked_add(u64::from(second))?
        .checked_mul(unit)?;
    u32::try_from(size.checked_sub(crop)?)
        .ok()
        .filter(|value| *value > 0)
}

fn skip_scaling_list(reader: &mut BitReader, size: usize) -> Option<()> {
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    for _ in 0..size {
        if next_scale != 0 {
            let delta = reader.read_se()?;
            if !(-128..=127).contains(&delta) {
                return None;
            }
            next_scale = last_scale.checked_add(delta)?.rem_euclid(256);
        }
        if next_scale != 0 {
            last_scale = next_scale;
        }
    }
    Some(())
}

fn vui_frame_rate(reader: &mut BitReader) -> Option<f64> {
    if reader.read_bits(1)? == 1 && reader.read_bits(8)? == 255 {
        reader.skip(32)?;
    }
    if reader.read_bits(1)? == 1 {
        reader.skip(1)?;
    }
    if reader.read_bits(1)? == 1 {
        reader.skip(4)?;
        if reader.read_bits(1)? == 1 {
            reader.skip(24)?;
        }
    }
    if reader.read_bits(1)? == 1 {
        reader.read_ue()?;
        reader.read_ue()?;
    }
    if reader.read_bits(1)? == 0 {
        return Some(0.0);
    }
    let num_units_in_tick = reader.read_bits(32)?;
    let time_scale = reader.read_bits(32)?;
    Some(if num_units_in_tick > 0 && time_scale > 0 {
        f64::from(time_scale) / (2.0 * f64::from(num_units_in_tick))
    } else {
        0.0
    })
}

pub fn signed_be24(value: i32) -> [u8; 3] {
    let [_, high, mid, low] = value.clamp(-8_388_608, 8_388_607).to_be_bytes();
    [high, mid, low]
}

pub fn take_u16_prefixed<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, tail) = rest.split_first_chunk::<2>()?;
    let (item, tail) = tail.split_at_checked(usize::from(u16::from_be_bytes(*len)))?;
    *rest = tail;
    Some(item)
}

pub fn aac_frequency_index(sample_rate: u32) -> Option<u8> {
    match sample_rate {
        96000 => Some(0),
        88200 => Some(1),
        64000 => Some(2),
        48000 => Some(3),
        44100 => Some(4),
        32000 => Some(5),
        24000 => Some(6),
        22050 => Some(7),
        16000 => Some(8),
        12000 => Some(9),
        11025 => Some(10),
        8000 => Some(11),
        7350 => Some(12),
        _ => None,
    }
}

pub fn build_aac_sequence_header(sample_rate: u32, channels: u32) -> Bytes {
    let freq_idx = aac_frequency_index(sample_rate).unwrap_or(3);
    let chan_cfg = channels.min(7) as u8;
    let audio_object_type: u8 = 2; // AAC-LC
    let asc_byte0 = (audio_object_type << 3) | (freq_idx >> 1);
    let asc_byte1 = ((freq_idx & 0x01) << 7) | (chan_cfg << 3);

    let mut out = Vec::with_capacity(4);
    out.extend_from_slice(&[0xAF, 0x00]);
    out.extend_from_slice(&[asc_byte0, asc_byte1]);
    Bytes::from(out)
}

#[derive(Debug)]
pub struct AvccRecord<'a> {
    pub length_size_minus_one: u8,
    pub sps: Vec<&'a [u8]>,
    pub pps: Vec<&'a [u8]>,
}

pub fn avcc_record(record: &[u8]) -> Option<AvccRecord<'_>> {
    let (
        &[
            _version,
            _profile,
            _compatibility,
            _level,
            length_byte,
            sps_byte,
        ],
        mut rest,
    ) = record.split_first_chunk::<6>()?;
    let mut sps = Vec::with_capacity(usize::from(sps_byte & 0x1F));
    for _ in 0..(sps_byte & 0x1F) {
        sps.push(take_u16_prefixed(&mut rest)?);
    }
    let (&pps_count, tail) = rest.split_first()?;
    rest = tail;
    let mut pps = Vec::with_capacity(usize::from(pps_count));
    for _ in 0..pps_count {
        pps.push(take_u16_prefixed(&mut rest)?);
    }
    Some(AvccRecord {
        length_size_minus_one: length_byte & 0x03,
        sps,
        pps,
    })
}

pub fn parse_avcc_config(data: &[u8]) -> (usize, Vec<u8>) {
    let Some(&length_byte) = data.get(4).filter(|_| data.len() >= 8) else {
        return (4, Vec::new());
    };
    let nalu_len_size = usize::from(length_byte & 0x03).saturating_add(1);
    let annexb = avcc_record(data)
        .map(|record| {
            let mut out = Vec::new();
            for nalu in record.sps.iter().chain(&record.pps) {
                out.extend_from_slice(&[0, 0, 0, 1]);
                out.extend_from_slice(nalu);
            }
            out
        })
        .unwrap_or_default();
    (nalu_len_size, annexb)
}

pub fn avcc_to_annexb(data: &[u8], nalu_len_size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    avcc_to_annexb_into(data, nalu_len_size, &mut out);
    out
}

#[inline]
pub fn avcc_to_annexb_into(data: &[u8], nalu_len_size: usize, out: &mut Vec<u8>) {
    if !(1..=4).contains(&nalu_len_size) {
        return;
    }
    let mut rest = data;
    while let Some((length, tail)) = rest.split_at_checked(nalu_len_size) {
        let nalu_len = length
            .iter()
            .fold(0usize, |len, &byte| (len << 8) | usize::from(byte));
        let Some((nalu, tail)) = tail.split_at_checked(nalu_len).filter(|_| nalu_len > 0) else {
            break;
        };
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nalu);
        rest = tail;
    }
}

pub fn h265_nal_type(nalu: &[u8]) -> Option<u8> {
    match nalu {
        [first, _, ..] => Some((first >> 1) & 0x3F),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HevcProfileTierLevel {
    pub profile_space: u8,
    pub tier_flag: bool,
    pub profile_idc: u8,
    pub profile_compatibility_flags: u32,
    pub constraint_indicator_flags: u64,
    pub level_idc: u8,
    pub width: u32,
    pub height: u32,
    pub max_sub_layers_minus1: u8,
    pub temporal_id_nested: bool,
    pub chroma_format_idc: u8,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
}

pub fn parse_hevc_profile_tier_level(sps: &[u8]) -> Option<HevcProfileTierLevel> {
    let payload = sps.get(2..).filter(|payload| !payload.is_empty())?;
    let rbsp = rbsp(payload);
    let mut reader = BitReader::new(&rbsp);
    let _sps_video_parameter_set_id = reader.read_bits(4)?;
    let max_sub_layers_minus1 = u8::try_from(reader.read_bits(3)?).ok()?;
    let temporal_id_nested = reader.read_bits(1)? == 1;
    let profile_space = u8::try_from(reader.read_bits(2)?).ok()?;
    let tier_flag = reader.read_bits(1)? == 1;
    let profile_idc = u8::try_from(reader.read_bits(5)?).ok()?;
    let profile_compatibility_flags = reader.read_bits(32)?;
    let constraint_indicator_flags =
        (u64::from(reader.read_bits(16)?) << 32) | u64::from(reader.read_bits(32)?);
    let level_idc = u8::try_from(reader.read_bits(8)?).ok()?;
    skip_hevc_sub_layer_profile_tier_level(&mut reader, max_sub_layers_minus1)?;
    let _sps_seq_parameter_set_id = reader.read_ue()?;
    let chroma_format_idc = u8::try_from(reader.read_ue()?).ok()?;
    if chroma_format_idc > 3 {
        return None;
    }
    if chroma_format_idc == 3 {
        let _separate_colour_plane_flag = reader.read_bits(1)?;
    }
    let pic_width = reader.read_ue()?;
    let pic_height = reader.read_ue()?;
    let (mut crop_x, mut crop_y) = (0u32, 0u32);
    if reader.read_bits(1)? == 1 {
        let left = reader.read_ue()?;
        let right = reader.read_ue()?;
        let top = reader.read_ue()?;
        let bottom = reader.read_ue()?;
        crop_x = left.saturating_add(right);
        crop_y = top.saturating_add(bottom);
    }
    let sub_width = if matches!(chroma_format_idc, 1 | 2) {
        2
    } else {
        1
    };
    let sub_height = if chroma_format_idc == 1 { 2 } else { 1 };
    let width = pic_width.saturating_sub(crop_x.saturating_mul(sub_width));
    let height = pic_height.saturating_sub(crop_y.saturating_mul(sub_height));
    let bit_depth_luma_minus8 = u8::try_from(reader.read_ue()?).ok()?;
    let bit_depth_chroma_minus8 = u8::try_from(reader.read_ue()?).ok()?;
    if bit_depth_luma_minus8 > 7 || bit_depth_chroma_minus8 > 7 {
        return None;
    }

    Some(HevcProfileTierLevel {
        profile_space,
        tier_flag,
        profile_idc,
        profile_compatibility_flags,
        constraint_indicator_flags,
        level_idc,
        width,
        height,
        max_sub_layers_minus1,
        temporal_id_nested,
        chroma_format_idc,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
    })
}

fn skip_hevc_sub_layer_profile_tier_level(
    reader: &mut BitReader<'_>,
    max_sub_layers_minus1: u8,
) -> Option<()> {
    let sub_layers = usize::from(max_sub_layers_minus1);
    let mut present = [(false, false); 7];
    for (profile_present, level_present) in present.iter_mut().take(sub_layers) {
        *profile_present = reader.read_bits(1)? == 1;
        *level_present = reader.read_bits(1)? == 1;
    }
    if sub_layers > 0 {
        for _ in sub_layers..8 {
            let _reserved_zero_2bits = reader.read_bits(2)?;
        }
    }
    for &(profile_present, level_present) in present.iter().take(sub_layers) {
        if profile_present {
            let _sub_layer_profile_space = reader.read_bits(2)?;
            let _sub_layer_tier_flag = reader.read_bits(1)?;
            let _sub_layer_profile_idc = reader.read_bits(5)?;
            let _sub_layer_profile_compatibility_flags = reader.read_bits(32)?;
            let _sub_layer_progressive_source_flag = reader.read_bits(1)?;
            let _sub_layer_interlaced_source_flag = reader.read_bits(1)?;
            let _sub_layer_non_packed_constraint_flag = reader.read_bits(1)?;
            let _sub_layer_frame_only_constraint_flag = reader.read_bits(1)?;
            reader.skip(44)?;
        }
        if level_present {
            let _sub_layer_level_idc = reader.read_bits(8)?;
        }
    }
    Some(())
}

pub fn build_hvcc_record(annexb_data: &[u8]) -> Option<Bytes> {
    let nalus = split_annexb_nalus(annexb_data);
    let first_of_type = |expected: u8| -> Option<&[u8]> {
        nalus
            .iter()
            .copied()
            .find(|n| h265_nal_type(n) == Some(expected))
    };
    let vps = first_of_type(32)?;
    let sps = first_of_type(33)?;
    let pps = first_of_type(34)?;
    let profile = parse_hevc_profile_tier_level(sps)?;

    let nalu_bytes = vps
        .len()
        .saturating_add(sps.len())
        .saturating_add(pps.len());
    let mut hvcc = Vec::with_capacity(nalu_bytes.saturating_add(64));
    hvcc.push(1);
    hvcc.push(
        (profile.profile_space << 6) | (u8::from(profile.tier_flag) << 5) | profile.profile_idc,
    );
    hvcc.extend_from_slice(&profile.profile_compatibility_flags.to_be_bytes());
    let [_, _, constraint_indicator_flags @ ..] = profile.constraint_indicator_flags.to_be_bytes();
    hvcc.extend_from_slice(&constraint_indicator_flags);
    hvcc.push(profile.level_idc);
    hvcc.extend_from_slice(&[0xF0, 0x00, 0xFC]);
    hvcc.push(0xFC | (profile.chroma_format_idc & 0x03));
    hvcc.push(0xF8 | (profile.bit_depth_luma_minus8 & 0x07));
    hvcc.push(0xF8 | (profile.bit_depth_chroma_minus8 & 0x07));
    hvcc.extend_from_slice(&[0x00, 0x00]);
    let temporal_layers = profile.max_sub_layers_minus1.saturating_add(1).min(7);
    hvcc.push((temporal_layers << 3) | (u8::from(profile.temporal_id_nested) << 2) | 3);
    hvcc.push(3);
    let push_array = |out: &mut Vec<u8>, nal_type: u8, nalu: &[u8]| -> Option<()> {
        let len = u16::try_from(nalu.len()).ok()?;
        out.push(0x80 | nal_type);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(nalu);
        Some(())
    };
    push_array(&mut hvcc, 32, vps)?;
    push_array(&mut hvcc, 33, sps)?;
    push_array(&mut hvcc, 34, pps)?;
    Some(Bytes::from(hvcc))
}
