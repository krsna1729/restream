#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
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
                prepend_adts(payload, sample_rate, channels).map(Cow::Owned)
            }
        }
        PayloadFormat::Flv => {
            let raw_aac = flv_aac_frame(payload)?;
            prepend_adts(raw_aac, sample_rate, channels).map(Cow::Owned)
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
                prepend_adts_into(payload, sample_rate, channels, buf)?;
                Some(buf.as_slice())
            }
        }
        PayloadFormat::Flv => {
            let raw_aac = flv_aac_frame(payload)?;
            buf.clear();
            prepend_adts_into(raw_aac, sample_rate, channels, buf)?;
            Some(buf.as_slice())
        }
    }
}

/// The raw AAC frame of an FLV AAC data tag: `None` for a sequence header
/// (packet type 0) or a tag with no frame bytes.
fn flv_aac_frame(payload: &[u8]) -> Option<&[u8]> {
    match payload {
        [_, packet_type, frame @ ..] if *packet_type != 0 && !frame.is_empty() => Some(frame),
        _ => None,
    }
}

/// Prepare a Raw audio payload for RTMP publishing.
///
/// Strips ADTS header if present, prepends 2-byte FLV audio header `[0xAF, 0x01]`.
pub fn audio_for_rtmp(payload: &[u8]) -> Vec<u8> {
    let raw = strip_adts(payload);
    let mut out = Vec::with_capacity(raw.len().saturating_add(2));
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
    out.reserve(raw.len().saturating_add(2));
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

/// The largest raw AAC frame an ADTS header can describe: `aac_frame_length`
/// is 13 bits and counts the 7 header bytes.
pub const MAX_ADTS_PAYLOAD: usize = 0x1FFF - 7;

/// Build a 7-byte ADTS header for an AAC frame of `frame_len` bytes, or
/// `None` above [`MAX_ADTS_PAYLOAD`], where the length field cannot hold it.
pub fn build_adts_header(frame_len: usize, sample_rate: u32, channels: u32) -> Option<[u8; 7]> {
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
    let total_len = u16::try_from(frame_len)
        .ok()
        .and_then(|len| len.checked_add(7))
        .filter(|&len| len <= 0x1FFF)?;
    let [len_high, len_low] = total_len.to_be_bytes();
    Some([
        0xFF,
        0xF1,                                                // MPEG-4, Layer 0, no CRC
        (1 << 6) | (freq_idx << 2) | (chan_cfg >> 2),        // AAC-LC profile
        ((chan_cfg & 0x03) << 6) | ((len_high >> 3) & 0x03), // length bits 12..11
        (len_high << 5) | (len_low >> 3),                    // length bits 10..3
        ((len_low & 0x07) << 5) | 0x1F,                      // length bits 2..0
        0xFC,
    ])
}

pub(super) fn has_adts_sync(data: &[u8]) -> bool {
    matches!(data, [0xFF, second, ..] if second & 0xF0 == 0xF0)
}

/// Count complete AAC ADTS frames in a payload.
pub fn adts_frame_count(data: &[u8]) -> usize {
    let mut rest = data;
    let mut count = 0usize;
    while let Some(&[0xFF, sync_low, _, b3, b4, b5, _]) = rest.first_chunk::<7>() {
        if sync_low & 0xF0 != 0xF0 {
            break;
        }
        let frame_len =
            (usize::from(b3 & 0x03) << 11) | (usize::from(b4) << 3) | (usize::from(b5 & 0xE0) >> 5);
        let Some(tail) = rest.get(frame_len..).filter(|_| frame_len >= 7) else {
            break;
        };
        count = count.saturating_add(1);
        rest = tail;
    }
    count
}

/// `raw_aac` behind an ADTS header, or `None` when it is too long for one.
pub(super) fn prepend_adts(raw_aac: &[u8], sample_rate: u32, channels: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    prepend_adts_into(raw_aac, sample_rate, channels, &mut out)?;
    Some(out)
}

/// Like [`prepend_adts`] but writes into a caller-provided reusable buffer.
fn prepend_adts_into(
    raw_aac: &[u8],
    sample_rate: u32,
    channels: u32,
    out: &mut Vec<u8>,
) -> Option<()> {
    let adts = build_adts_header(raw_aac.len(), sample_rate, channels)?;
    out.reserve(raw_aac.len().saturating_add(7));
    out.extend_from_slice(&adts);
    out.extend_from_slice(raw_aac);
    Some(())
}

/// Strip ADTS header if present, returning the raw AAC frame data.
pub fn strip_adts(data: &[u8]) -> &[u8] {
    if let [0xFF, second, _, _, _, _, _, ..] = *data
        && second & 0xF0 == 0xF0
    {
        // protection_absent bit (byte 1, bit 0): 1 = no CRC (7-byte header), 0 = CRC (9-byte)
        let hdr_len = if second & 0x01 == 1 { 7 } else { 9 };
        if data.len() > hdr_len {
            return data.get(hdr_len..).unwrap_or(data);
        }
    }
    data
}
