#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use restream_core::PayloadFormat;

/// Prepare an audio payload for MPEG-TS muxing (ADTS-wrapped output).
///
/// - **FLV**: strips the 2-byte header, skips config packets (packet_type 0),
///   and writes a 7-byte ADTS header plus the raw AAC frame into `buf`.
/// - **Raw with ADTS** (from SRT ingest): returns `Some(payload)` — zero-copy.
/// - **Raw without ADTS** (from transcoder/FFmpeg): writes ADTS + frame into `buf`.
///
/// `buf` is cleared before it is written. Returns `None` for config/sequence
/// packets and frames the ADTS length field cannot hold.
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

use restream_core::codec::{has_adts_sync, prepend_adts_into};
