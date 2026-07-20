use crate::media::codec;
use crate::media::metadata::{AudioMeta, VideoMeta};

pub(super) fn parse_flv_video_meta(data: &[u8]) -> Option<VideoMeta> {
    if data.len() < 2 {
        return None;
    }
    let codec_id = data[0] & 0x0F;
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
    if codec_id == 7 && data[1] == 0 && data.len() > 12 {
        let avc_config = &data[5..];
        if avc_config.len() >= 4 {
            let profile_idc = avc_config[1];
            let level_idc = avc_config[3];
            meta.profile = Some(
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
                .to_string(),
            );
            meta.level = Some(format!("{}.{}", level_idc / 10, level_idc % 10));

            // Parse SPS for resolution and timing info.
            if avc_config.len() > 8 {
                let num_sps = (avc_config[5] & 0x1F) as usize;
                if num_sps > 0 && avc_config.len() > 8 {
                    let sps_len = ((avc_config[6] as usize) << 8) | (avc_config[7] as usize);
                    if avc_config.len() >= 8 + sps_len
                        && sps_len > 1
                        && let Some(info) = parse_sps_video_info(&avc_config[8..8 + sps_len])
                    {
                        meta.width = info.width;
                        meta.height = info.height;
                        meta.fps = info.fps;
                    }
                }
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
    if data.len() <= 5 || (data[0] & 0x0F) != 7 || data[1] != 0 {
        return None;
    }
    let avc_config = &data[5..];
    if avc_config.len() < 6 {
        return None;
    }

    let mut annexb = Vec::new();
    let num_sps = (avc_config[5] & 0x1F) as usize;
    let mut offset = 6usize;
    for _ in 0..num_sps {
        let len = *avc_config.get(offset)? as usize;
        let len = (len << 8) | (*avc_config.get(offset + 1)? as usize);
        offset += 2;
        let sps = avc_config.get(offset..offset + len)?;
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(sps);
        offset += len;
    }

    let num_pps = *avc_config.get(offset)? as usize;
    offset += 1;
    for _ in 0..num_pps {
        let len = *avc_config.get(offset)? as usize;
        let len = (len << 8) | (*avc_config.get(offset + 1)? as usize);
        offset += 2;
        let pps = avc_config.get(offset..offset + len)?;
        annexb.extend_from_slice(&[0, 0, 0, 1]);
        annexb.extend_from_slice(pps);
        offset += len;
    }

    codec::annexb_parameter_sets(&annexb)
}

pub(super) fn flv_video_composition_time_ms(data: &[u8]) -> i32 {
    if data.len() < 5 || !matches!(data[0] & 0x0f, 7 | 12) || data[1] != 1 {
        return 0;
    }

    let value = ((data[2] as i32) << 16) | ((data[3] as i32) << 8) | data[4] as i32;
    if value & 0x0080_0000 != 0 {
        value | !0x00ff_ffff
    } else {
        value
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FlvVideoPacketKind {
    SequenceHeader,
    Keyframe,
    Interframe,
}

pub(super) fn classify_flv_video_packet(data: &[u8]) -> Option<FlvVideoPacketKind> {
    if data.len() < 2 || !matches!(data[0] & 0x0f, 7 | 12) {
        return None;
    }

    if data[1] == 0 {
        return Some(FlvVideoPacketKind::SequenceHeader);
    }

    Some(if (data[0] >> 4) == 1 {
        FlvVideoPacketKind::Keyframe
    } else {
        FlvVideoPacketKind::Interframe
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SpsVideoInfo {
    pub(super) width: u32,
    height: u32,
    fps: f64,
}

pub(super) fn sps_dimensions(
    pic_width: u32,
    pic_height: u32,
    frame_mbs_only: u32,
    crop_left: u32,
    crop_right: u32,
    crop_top: u32,
    crop_bottom: u32,
) -> Option<(u32, u32)> {
    if frame_mbs_only > 1 {
        return None;
    }

    let frame_mbs_factor = 2u64.checked_sub(frame_mbs_only as u64)?;

    let width_base = (pic_width as u64).checked_add(1)?.checked_mul(16)?;
    let width_crop = (crop_left as u64)
        .checked_add(crop_right as u64)?
        .checked_mul(2)?;
    if width_base <= width_crop {
        return None;
    }
    let width = u32::try_from(width_base - width_crop).ok()?;

    let height_base = (pic_height as u64)
        .checked_add(1)?
        .checked_mul(frame_mbs_factor)?
        .checked_mul(16)?;
    let height_crop = (crop_top as u64)
        .checked_add(crop_bottom as u64)?
        .checked_mul(2)?;
    if height_base <= height_crop {
        return None;
    }
    let height = u32::try_from(height_base - height_crop).ok()?;

    Some((width, height))
}

pub(super) fn parse_sps_video_info(sps_nalu: &[u8]) -> Option<SpsVideoInfo> {
    if sps_nalu.is_empty() {
        return None;
    }
    // Remove emulation prevention bytes (0x00 0x00 0x03 → 0x00 0x00)
    let mut rbsp = Vec::with_capacity(sps_nalu.len());
    let mut i = 0;
    while i < sps_nalu.len() {
        if i + 2 < sps_nalu.len()
            && sps_nalu[i] == 0
            && sps_nalu[i + 1] == 0
            && sps_nalu[i + 2] == 3
        {
            rbsp.push(0);
            rbsp.push(0);
            i += 3;
        } else {
            rbsp.push(sps_nalu[i]);
            i += 1;
        }
    }

    let mut reader = BitReader::new(&rbsp);
    // Skip NAL unit header byte
    reader.skip(8)?;
    let profile_idc = reader.read_bits(8)? as u8;
    reader.skip(8)?; // constraint flags
    reader.skip(8)?; // level_idc
    reader.read_exp_golomb()?; // seq_parameter_set_id

    let high_profiles: &[u8] = &[100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134];
    if high_profiles.contains(&profile_idc) {
        let chroma = reader.read_exp_golomb()?;
        if chroma == 3 {
            reader.skip(1)?; // separate_colour_plane_flag
        }
        reader.read_exp_golomb()?; // bit_depth_luma_minus8
        reader.read_exp_golomb()?; // bit_depth_chroma_minus8
        reader.skip(1)?; // qpprime_y_zero_transform_bypass_flag
        let scaling_present = reader.read_bits(1)?;
        if scaling_present == 1 {
            let count = if chroma != 3 { 8 } else { 12 };
            for j in 0..count {
                let list_present = reader.read_bits(1)?;
                if list_present == 1 {
                    let size = if j < 6 { 16 } else { 64 };
                    let mut last_scale = 8i32;
                    let mut next_scale = 8i32;
                    for _ in 0..size {
                        if next_scale != 0 {
                            let delta = reader.read_signed_exp_golomb()?;
                            next_scale = (last_scale + delta + 256) % 256;
                        }
                        last_scale = if next_scale == 0 {
                            last_scale
                        } else {
                            next_scale
                        };
                    }
                }
            }
        }
    }

    reader.read_exp_golomb()?; // log2_max_frame_num_minus4
    let poc_type = reader.read_exp_golomb()?;
    if poc_type == 0 {
        reader.read_exp_golomb()?; // log2_max_pic_order_cnt_lsb_minus4
    } else if poc_type == 1 {
        reader.skip(1)?;
        reader.read_signed_exp_golomb()?;
        reader.read_signed_exp_golomb()?;
        let n = reader.read_exp_golomb()?;
        for _ in 0..n {
            reader.read_signed_exp_golomb()?;
        }
    }
    reader.read_exp_golomb()?; // max_num_ref_frames
    reader.skip(1)?; // gaps_in_frame_num_value_allowed_flag

    let pic_width = reader.read_exp_golomb()?;
    let pic_height = reader.read_exp_golomb()?;
    let frame_mbs_only = reader.read_bits(1)?;
    if frame_mbs_only == 0 {
        reader.skip(1)?; // mb_adaptive_frame_field_flag
    }
    reader.skip(1)?; // direct_8x8_inference_flag
    let crop_flag = reader.read_bits(1)?;
    let (crop_left, crop_right, crop_top, crop_bottom) = if crop_flag == 1 {
        (
            reader.read_exp_golomb()?,
            reader.read_exp_golomb()?,
            reader.read_exp_golomb()?,
            reader.read_exp_golomb()?,
        )
    } else {
        (0, 0, 0, 0)
    };

    let (width, height) = sps_dimensions(
        pic_width,
        pic_height,
        frame_mbs_only,
        crop_left,
        crop_right,
        crop_top,
        crop_bottom,
    )?;
    let mut info = SpsVideoInfo {
        width,
        height,
        fps: 0.0,
    };

    if let Some(vui_present) = reader.read_bits(1)
        && vui_present == 1
    {
        if reader.read_bits(1)? == 1 {
            let sar_idx = reader.read_bits(8)?;
            if sar_idx == 255 {
                reader.skip(32)?;
            }
        }
        if reader.read_bits(1)? == 1 {
            reader.skip(1)?;
        }
        if reader.read_bits(1)? == 1 {
            reader.skip(3)?;
            reader.skip(1)?;
            if reader.read_bits(1)? == 1 {
                reader.skip(24)?;
            }
        }
        if reader.read_bits(1)? == 1 {
            reader.read_exp_golomb()?;
            reader.read_exp_golomb()?;
        }
        if reader.read_bits(1)? == 1 {
            let num_units_in_tick = reader.read_bits(32)?;
            let time_scale = reader.read_bits(32)?;
            let fixed_frame_rate_flag = reader.read_bits(1)?;
            if num_units_in_tick > 0 && time_scale > 0 {
                let fps = time_scale as f64 / (2.0 * num_units_in_tick as f64);
                if fps.is_finite() && fps > 0.0 {
                    info.fps = if fixed_frame_rate_flag == 1 {
                        fps
                    } else {
                        fps.max(0.0)
                    };
                }
            }
        }
    }

    Some(info)
}

pub(super) struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8, // 0..8, bits consumed in current byte
}

impl<'a> BitReader<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn read_bits(&mut self, n: u8) -> Option<u32> {
        let mut val = 0u32;
        for _ in 0..n {
            if self.byte_pos >= self.data.len() {
                return None;
            }
            val = (val << 1) | ((self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1) as u32;
            self.bit_pos += 1;
            if self.bit_pos >= 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }
        Some(val)
    }

    fn skip(&mut self, n: u8) -> Option<()> {
        self.read_bits(n).map(|_| ())
    }

    pub(super) fn read_exp_golomb(&mut self) -> Option<u32> {
        let mut zeros = 0u32;
        loop {
            let bit = self.read_bits(1)?;
            if bit == 1 {
                break;
            }
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        if zeros == 0 {
            return Some(0);
        }
        let suffix = self.read_bits(zeros as u8)?;
        Some((1 << zeros) - 1 + suffix)
    }

    fn read_signed_exp_golomb(&mut self) -> Option<i32> {
        let val = self.read_exp_golomb()?;
        if val == 0 {
            Some(0)
        } else if val % 2 == 1 {
            Some((val / 2 + 1) as i32)
        } else {
            Some(-(val as i32 / 2))
        }
    }
}

pub(super) fn parse_flv_audio_meta(data: &[u8]) -> Option<AudioMeta> {
    if data.is_empty() {
        return None;
    }
    let byte0 = data[0];
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
    let channels = channels_id as u32 + 1;

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
    if format_id == 10 && data.len() > 2 && data[1] == 0 {
        let asc = &data[2..];
        if asc.len() >= 2 {
            let audio_object_type = asc[0] >> 3;
            meta.profile = match audio_object_type {
                1 => Some("Main".to_string()),
                2 => Some("LC".to_string()),
                3 => Some("SSR".to_string()),
                4 => Some("LTP".to_string()),
                5 => Some("SBR".to_string()),
                _ => Some(format!("AAC Profile {}", audio_object_type)),
            };
            let freq_idx = ((asc[0] & 0x07) << 1) | (asc[1] >> 7);
            let ch_config = (asc[1] >> 3) & 0x0F;
            let aac_rates: &[u32] = &[
                96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000,
                7350,
            ];
            if (freq_idx as usize) < aac_rates.len() {
                meta.sample_rate = aac_rates[freq_idx as usize];
            }
            if ch_config > 0 {
                meta.channels = ch_config as u32;
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
