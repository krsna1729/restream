#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use std::borrow::Cow;

use restream_core::PayloadFormat;

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
    let mut out = Vec::new();
    audio_for_rtmp_into(payload, &mut out);
    out
}

use restream_core::codec::audio_for_rtmp_into;
use restream_core::codec::{has_adts_sync, prepend_adts, prepend_adts_into};
