use std::borrow::Cow;

use bytes::Bytes;

use crate::media::packet::PayloadFormat;

/// Prepare an audio payload for MPEG-TS muxing (ADTS-wrapped output).
///
/// - **FLV**: strips 2-byte header, skips config packets (packet_type 0),
///   prepends a 7-byte ADTS header to the raw AAC frame.
/// - **Raw with ADTS** (from SRT ingest): pass-through.
/// - **Raw without ADTS** (from transcoder/FFmpeg): prepends ADTS header.
pub fn audio_for_ts<'a>(
    payload: &'a [u8],
    format: PayloadFormat,
    sample_rate: u32,
    channels: u32,
) -> Option<Cow<'a, [u8]>> {
    match format {
        PayloadFormat::Raw => {
            if payload.is_empty() {
                return None;
            }
            if has_adts_sync(payload) {
                Some(Cow::Borrowed(payload))
            } else {
                Some(Cow::Owned(prepend_adts(payload, sample_rate, channels)))
            }
        }
        PayloadFormat::Flv => {
            if payload.len() <= 2 || payload[1] == 0 {
                return None;
            }
            let raw_aac = &payload[2..];
            Some(Cow::Owned(prepend_adts(raw_aac, sample_rate, channels)))
        }
    }
}

/// Zero-allocation variant of [`audio_for_ts`].
///
/// For `Raw` with ADTS: returns `Some(payload)` directly — zero-copy.
/// All other cases write into `buf` (cleared first) and return `Some(&buf[..])`.
/// Returns `None` for config/sequence packets.
#[inline]
pub fn audio_for_ts_into<'a>(
    payload: &'a [u8],
    format: PayloadFormat,
    sample_rate: u32,
    channels: u32,
    buf: &'a mut Vec<u8>,
) -> Option<&'a [u8]> {
    match format {
        PayloadFormat::Raw => {
            if payload.is_empty() {
                return None;
            }
            if has_adts_sync(payload) {
                Some(payload) // zero-copy, buf untouched
            } else {
                buf.clear();
                prepend_adts_into(payload, sample_rate, channels, buf);
                Some(buf.as_slice())
            }
        }
        PayloadFormat::Flv => {
            if payload.len() <= 2 || payload[1] == 0 {
                return None;
            }
            let raw_aac = &payload[2..];
            buf.clear();
            prepend_adts_into(raw_aac, sample_rate, channels, buf);
            Some(buf.as_slice())
        }
    }
}

/// Prepare a Raw audio payload for RTMP publishing.
///
/// Strips ADTS header if present, prepends 2-byte FLV audio header `[0xAF, 0x01]`.
pub fn audio_for_rtmp(payload: &[u8]) -> Vec<u8> {
    let raw = strip_adts(payload);
    let mut out = Vec::with_capacity(raw.len() + 2);
    out.extend_from_slice(&[0xAF, 0x01]);
    out.extend_from_slice(raw);
    out
}

/// Zero-allocation variant of [`audio_for_rtmp`].
///
/// Clears `out` and writes the FLV-wrapped raw AAC into it in-place.
#[inline]
pub fn audio_for_rtmp_into(payload: &[u8], out: &mut Vec<u8>) {
    let raw = strip_adts(payload);
    out.clear();
    out.reserve(raw.len() + 2);
    out.extend_from_slice(&[0xAF, 0x01]);
    out.extend_from_slice(raw);
}

/// Build an FLV audio sequence header (AudioSpecificConfig) from sample rate
/// and channel count. Used for the SRT→RTMP Raw path where no cached
/// AudioSpecificConfig exists — the 2-byte config is synthesized from the
/// audio metadata that is always available.
pub fn build_aac_sequence_header(sample_rate: u32, channels: u32) -> Bytes {
    let freq_idx: u8 = match sample_rate {
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        12000 => 9,
        11025 => 10,
        8000 => 11,
        _ => 3,
    };
    let chan_cfg = channels.min(7) as u8;
    let audio_object_type: u8 = 2; // AAC-LC

    // AudioSpecificConfig (2 bytes for AAC-LC without extension)
    // byte0: bits[7:3] = audioObjectType, bits[2:0] = samplingFrequencyIndex top 3 bits
    let asc_byte0 = (audio_object_type << 3) | (freq_idx >> 1);
    // byte1: bit[7] = samplingFrequencyIndex bottom bit, bits[6:3] = channelConfiguration
    let asc_byte1 = ((freq_idx & 0x01) << 7) | (chan_cfg << 3);

    let mut out = Vec::with_capacity(4);
    // FLV audio tag: AAC (0xAF) + packet_type=0 (sequence header)
    out.extend_from_slice(&[0xAF, 0x00]);
    out.extend_from_slice(&[asc_byte0, asc_byte1]);
    Bytes::from(out)
}

/// Build a 7-byte ADTS header for an AAC frame.
pub fn build_adts_header(frame_len: usize, sample_rate: u32, channels: u32) -> [u8; 7] {
    let freq_idx: u8 = match sample_rate {
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        12000 => 9,
        11025 => 10,
        8000 => 11,
        _ => 3,
    };
    let chan_cfg = channels.min(7) as u8;
    let total_len = (frame_len + 7) as u16;
    let mut hdr = [0u8; 7];
    hdr[0] = 0xFF;
    hdr[1] = 0xF1; // MPEG-4, Layer 0, no CRC
    hdr[2] = (1 << 6) | (freq_idx << 2) | (chan_cfg >> 2); // AAC-LC profile
    hdr[3] = ((chan_cfg & 0x03) << 6) | ((total_len >> 11) as u8 & 0x03);
    hdr[4] = (total_len >> 3) as u8;
    hdr[5] = ((total_len & 0x07) << 5) as u8 | 0x1F;
    hdr[6] = 0xFC;
    hdr
}

pub(super) fn has_adts_sync(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0xFF && (data[1] & 0xF0) == 0xF0
}

/// Count complete AAC ADTS frames in a payload.
pub fn adts_frame_count(data: &[u8]) -> usize {
    let mut pos = 0usize;
    let mut count = 0usize;
    while pos + 7 <= data.len() {
        let frame = &data[pos..];
        if !has_adts_sync(frame) {
            break;
        }
        let frame_len = (((frame[3] & 0x03) as usize) << 11)
            | ((frame[4] as usize) << 3)
            | (((frame[5] & 0xE0) as usize) >> 5);
        if frame_len < 7 || pos + frame_len > data.len() {
            break;
        }
        count += 1;
        pos += frame_len;
    }
    count
}

pub(super) fn prepend_adts(raw_aac: &[u8], sample_rate: u32, channels: u32) -> Vec<u8> {
    let adts = build_adts_header(raw_aac.len(), sample_rate, channels);
    let mut out = Vec::with_capacity(7 + raw_aac.len());
    out.extend_from_slice(&adts);
    out.extend_from_slice(raw_aac);
    out
}

/// Like [`prepend_adts`] but writes into a caller-provided reusable buffer.
fn prepend_adts_into(raw_aac: &[u8], sample_rate: u32, channels: u32, out: &mut Vec<u8>) {
    let adts = build_adts_header(raw_aac.len(), sample_rate, channels);
    out.reserve(7 + raw_aac.len());
    out.extend_from_slice(&adts);
    out.extend_from_slice(raw_aac);
}

/// Strip ADTS header if present, returning the raw AAC frame data.
pub fn strip_adts(data: &[u8]) -> &[u8] {
    if has_adts_sync(data) && data.len() >= 7 {
        // protection_absent bit (byte 1, bit 0): 1 = no CRC (7-byte header), 0 = CRC (9-byte)
        let hdr_len = if data[1] & 0x01 == 1 { 7 } else { 9 };
        if data.len() > hdr_len {
            return &data[hdr_len..];
        }
    }
    data
}
