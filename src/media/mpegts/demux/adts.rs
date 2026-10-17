//! ADTS framing of a TS audio stream: one AAC frame per unit, whatever the
//! PES boundaries. Most muxers start and end each audio PES on a frame
//! (`data_alignment_indicator`); some cut the stream anywhere, so a frame can
//! begin in one PES and end in the next, and a PES can begin mid-frame.

use super::{FRAME_TICKS_90K, ts_to_ms};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use bytes::Bytes;
use tracing::{debug, warn};

const ADTS_HEADER_LEN: usize = 7;
/// The 13-bit ADTS frame length's ceiling.
const MAX_ADTS_FRAME: usize = 8191;

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
    /// Bytes dropped before this stream framed anything: past two whole
    /// frames' worth, it carries no ADTS at all (raw AAC mislabelled as
    /// stream type 0x0F), and that is said once.
    unframed: usize,
    framed: bool,
}

/// What the next PES made of a carried frame start.
enum Carried {
    /// Completed by the first `consumed` bytes of the PES, and followed by a
    /// frame or by the PES end.
    Completed { frame: Vec<u8>, consumed: usize },
    /// Still incomplete: the whole PES joined the carry.
    Pending,
    /// The PES does not continue it (data was lost): dropped.
    Rejected,
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
        let data = payload;
        let mut first = None;
        let mut expected = self.next;
        let mut resume = None;
        if !self.carry.is_empty() {
            match self.complete_carry(&data) {
                Carried::Completed { frame, consumed } => {
                    // The carried frame keeps the time it started with.
                    match (expected, step_90k(&frame)) {
                        (Some((pts, dts)), Some(step)) => {
                            let unit = Bytes::from(frame);
                            first = Some(unit.clone());
                            push(output, track_index, pts, dts, unit);
                            expected = Some((pts.saturating_add(step), dts.saturating_add(step)));
                        }
                        _ => debug!(
                            track = track_index,
                            "a carried AAC frame with no time or rate to give it; dropped"
                        ),
                    }
                    resume = Some(consumed);
                }
                Carried::Pending => return Framed::Frames { first: None },
                Carried::Rejected => {
                    // Its time goes with it: a PES without a PTS must not
                    // take the dropped frame's slot.
                    expected = None;
                    debug!(
                        track = track_index,
                        "an AAC frame the last PES ended inside does not continue in this one; dropped"
                    );
                }
            }
        }
        let carried = resume.is_some();

        let start = match resume {
            Some(start) => start,
            // Stream type 0x0F is ADTS: bytes with no frame start in them
            // are a fragment (a stream joined mid-frame, or a frame whose
            // start was lost), never a unit of their own.
            None => match resync(&data) {
                Some(start) => start,
                None => {
                    self.next = expected;
                    self.drop_unframed(track_index, data.len());
                    return Framed::Frames { first };
                }
            },
        };
        let mut pes_first = pes_timestamps;
        let mut offset = start;
        while let Some(rest) = data.get(offset..).filter(|rest| !rest.is_empty()) {
            // The first frame that starts in this PES takes its timestamp.
            let at = pes_first.take().or(expected);
            let Some((pts, dts)) = at else {
                debug!(
                    track = track_index,
                    "AAC frame with no timestamp to give it; dropped"
                );
                break;
            };
            match frame_len(rest) {
                Some(len) if len <= rest.len() => {
                    let Some(step) = step_90k(rest) else {
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
                    push(output, track_index, pts, dts, unit);
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
        if first.is_some() {
            self.framed = true;
        }
        Framed::Frames { first }
    }

    /// Join the carried frame start with what of `data` completes it, copying
    /// only those bytes: a frame must end where another starts or at the PES
    /// end, and a PES that starts with a frame of its own (aligned) must
    /// complete it.
    fn complete_carry(&mut self, data: &[u8]) -> Carried {
        let aligned = frame_len(data).is_some();
        let carry = std::mem::take(&mut self.carry);
        // The header itself may be cut: read it across the two.
        let mut header = [0u8; ADTS_HEADER_LEN];
        let mut header_len: usize = 0;
        for (slot, byte) in header.iter_mut().zip(carry.iter().chain(data)) {
            *slot = *byte;
            header_len = header_len.saturating_add(1);
        }
        let header = header.get(..header_len).unwrap_or_default();
        match frame_len(header) {
            Some(len) => {
                let need = len.saturating_sub(carry.len());
                match (data.get(..need), data.get(need..)) {
                    (Some(rest), Some(after)) if after.is_empty() || sync_prefix(after) => {
                        let mut frame = carry;
                        frame.extend_from_slice(rest);
                        Carried::Completed {
                            frame,
                            consumed: need,
                        }
                    }
                    (None, _) if !aligned => {
                        self.carry = carry;
                        self.carry.extend_from_slice(data);
                        Carried::Pending
                    }
                    _ => Carried::Rejected,
                }
            }
            None if !aligned && header_len < ADTS_HEADER_LEN && sync_prefix(header) => {
                self.carry = carry;
                self.carry.extend_from_slice(data);
                Carried::Pending
            }
            None => Carried::Rejected,
        }
    }

    fn drop_unframed(&mut self, track_index: u32, dropped: usize) {
        if dropped == 0 {
            return;
        }
        if !self.framed {
            let before = self.unframed;
            self.unframed = self.unframed.saturating_add(dropped);
            if before <= 2 * MAX_ADTS_FRAME && self.unframed > 2 * MAX_ADTS_FRAME {
                warn!(
                    track = track_index,
                    "MPEG-TS AAC stream (type 0x0F) carries no ADTS frames; its audio is dropped"
                );
            }
        }
        debug!(
            track = track_index,
            dropped, "AAC PES holds no frame start; dropped"
        );
    }
}

fn push(output: &mut Vec<MediaPacket>, track_index: u32, pts: i64, dts: i64, payload: Bytes) {
    output.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index,
        pts: ts_to_ms(pts),
        dts: ts_to_ms(dts),
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload,
    });
}

/// 1024 samples in 90 kHz ticks, at the rate a frame's header names.
fn step_90k(frame: &[u8]) -> Option<i64> {
    crate::media::codec::adts_sample_rate(frame)
        .and_then(|rate| FRAME_TICKS_90K.checked_div(i64::from(rate)))
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
