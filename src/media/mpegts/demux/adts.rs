//! ADTS framing of a TS audio stream: one AAC frame per unit, whatever the
//! PES boundaries. Most muxers start and end each audio PES on a frame
//! (`data_alignment_indicator`); some cut the stream anywhere, so a frame can
//! begin in one PES and end in the next, and a PES can begin mid-frame.

use super::{FRAME_TICKS_90K, ts_to_ms};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use bytes::Bytes;
use tracing::debug;

const ADTS_HEADER_LEN: usize = 7;

/// A stream's ADTS framing state across its PES packets.
#[derive(Debug, Default)]
pub(in crate::media) struct AdtsFraming {
    /// The start of a frame the last PES ended inside: at most one frame,
    /// so at most 8191 bytes (the 13-bit frame length).
    carry: Vec<u8>,
    /// The 90 kHz (pts, dts) of the next frame: the carried one, else the one
    /// after the last frame emitted. A PES with no timestamp of its own (no
    /// frame starts in it, or its muxer left it out) continues from here.
    next: Option<(i64, i64)>,
}

/// What became of one PES.
pub(in crate::media) enum Framed {
    /// Its frames were pushed; `first` is the first, for the probe.
    Frames { first: Option<Bytes> },
    /// No known sample rate to step timestamps by: forward it whole, as one
    /// unit.
    Whole(Bytes),
}

impl AdtsFraming {
    /// Cut one PES payload (with its PES timestamps, when it carries them)
    /// into complete frames, each pushed with its own timestamp: the PES
    /// timestamp belongs to the first frame that starts in the PES, and each
    /// frame after steps by 1024 samples. A frame cut at the PES end is kept
    /// and completed by the next PES.
    pub(in crate::media) fn frame(
        &mut self,
        payload: Bytes,
        pes_timestamps: Option<(i64, i64)>,
        track_index: u32,
        output: &mut Vec<MediaPacket>,
    ) -> Framed {
        let mut data = payload;
        let mut carried = false;
        if !self.carry.is_empty() {
            // A PES that starts with a frame of its own is aligned: the
            // carried frame must end inside it, on that PES's frames.
            let aligned = frame_len(&data).is_some();
            let mut joined = std::mem::take(&mut self.carry);
            joined.extend_from_slice(&data);
            if continues(&joined, aligned) {
                data = Bytes::from(joined);
                carried = true;
            } else {
                debug!(
                    track = track_index,
                    "an AAC frame the last PES ended inside does not continue in this one; dropped"
                );
            }
        }

        let start = if carried {
            0
        } else {
            // Stream type 0x0F is ADTS: bytes with no frame start in them
            // are a fragment (a stream joined mid-frame, or a frame whose
            // start was lost), never a unit of their own.
            let Some(start) = resync(&data) else {
                if !data.is_empty() {
                    debug!(
                        track = track_index,
                        dropped = data.len(),
                        "AAC PES holds no frame start; dropped"
                    );
                }
                return Framed::Frames { first: None };
            };
            start
        };
        let mut first = None;
        let mut expected = self.next;
        let mut pes_first = pes_timestamps;
        let mut offset = start;
        while let Some(rest) = data.get(offset..).filter(|rest| !rest.is_empty()) {
            // The carried frame keeps its own time; the first frame that
            // starts in this PES takes the PES timestamp.
            let at = if carried && offset == 0 {
                expected
            } else {
                pes_first.take().or(expected)
            };
            let Some((pts, dts)) = at else {
                debug!(
                    track = track_index,
                    "AAC frame with no timestamp to give it; dropped"
                );
                break;
            };
            match frame_len(rest) {
                Some(len) if len <= rest.len() => {
                    let Some(step) = crate::media::codec::adts_sample_rate(rest)
                        .and_then(|rate| FRAME_TICKS_90K.checked_div(i64::from(rate)))
                    else {
                        // No known rate: its frames cannot be timed apart, so
                        // a PES of them goes whole, on its own timestamp.
                        if first.is_none() && !carried {
                            self.next = None;
                            return Framed::Whole(data.slice(start..));
                        }
                        debug!(
                            track = track_index,
                            dropped = rest.len(),
                            "AAC frames with no known sample rate after timed ones; dropped"
                        );
                        break;
                    };
                    let unit = data.slice(offset..offset.saturating_add(len));
                    if first.is_none() {
                        first = Some(unit.clone());
                    }
                    output.push(MediaPacket {
                        media_type: MediaType::Audio,
                        track_index,
                        pts: ts_to_ms(pts),
                        dts: ts_to_ms(dts),
                        is_keyframe: false,
                        format: PayloadFormat::Raw,
                        payload: unit,
                    });
                    expected = Some((pts.saturating_add(step), dts.saturating_add(step)));
                    offset = offset.saturating_add(len);
                }
                Some(_) => {
                    self.carry.extend_from_slice(rest);
                    self.next = Some((pts, dts));
                    return Framed::Frames { first };
                }
                None if rest.len() < ADTS_HEADER_LEN && sync_prefix(rest) => {
                    self.carry.extend_from_slice(rest);
                    self.next = Some((pts, dts));
                    return Framed::Frames { first };
                }
                None => {
                    debug!(
                        track = track_index,
                        dropped = rest.len(),
                        "AAC PES bytes after its last frame are not a frame; dropped"
                    );
                    break;
                }
            }
        }
        self.next = expected;
        Framed::Frames { first }
    }
}

/// The length an ADTS header at the start of `data` names: a 12-bit sync
/// word, layer 0, and a length that covers at least the header.
fn frame_len(data: &[u8]) -> Option<usize> {
    let &[0xFF, b1, _, b3, b4, b5, _] = data.first_chunk::<ADTS_HEADER_LEN>()? else {
        return None;
    };
    if b1 & 0xF6 != 0xF0 {
        return None;
    }
    let len =
        (usize::from(b3 & 0x03) << 11) | (usize::from(b4) << 3) | (usize::from(b5 & 0xE0) >> 5);
    (len >= ADTS_HEADER_LEN).then_some(len)
}

/// `data` could start a header: what is there of one matches.
fn sync_prefix(data: &[u8]) -> bool {
    match data {
        [0xFF] => true,
        [0xFF, b1, ..] => b1 & 0xF6 == 0xF0,
        _ => false,
    }
}

/// A carried frame start joined with the next PES continues as a frame: the
/// frame ends where another starts or at the end of the bytes, or (unless the
/// PES is `aligned`, starting with a frame of its own) it is still incomplete.
fn continues(joined: &[u8], aligned: bool) -> bool {
    match frame_len(joined) {
        Some(len) => match joined.get(len..) {
            Some(after) => after.is_empty() || sync_prefix(after),
            None => !aligned,
        },
        None => !aligned && joined.len() < ADTS_HEADER_LEN && sync_prefix(joined),
    }
}

/// Where the first frame in `data` starts: a header whose frame ends where
/// another starts, at the end of `data`, or past it; or the start of a header
/// that `data` ends inside. Checking the next
/// header keeps a stray 0xFFF in a frame's payload from passing for one.
fn resync(data: &[u8]) -> Option<usize> {
    (0..data.len()).find(|&start| {
        let Some(rest) = data.get(start..) else {
            return false;
        };
        match frame_len(rest) {
            Some(len) => rest
                .get(len..)
                .is_none_or(|after| after.is_empty() || sync_prefix(after)),
            // A header cut at the end of the PES: carried, and checked when
            // the next PES completes it.
            None => rest.len() < ADTS_HEADER_LEN && sync_prefix(rest),
        }
    })
}
