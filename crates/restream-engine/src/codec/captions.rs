//! Closed captions in a video frame: CEA-608 and CEA-708 carried as ATSC
//! A/53 Part 4 user data (`GA94`, ITU-T T.35 registered) in SEI. Encoders
//! put SEI before the frame's first picture slice, so the walk stops there
//! and never scans the picture data. Reads only; nothing is rewritten.

use memchr::memmem;

use restream_core::PayloadFormat;
use restream_core::codec::rbsp;
use restream_rtmp::enhanced_rtmp_hevc::{EX_CODED_FRAMES, EX_CODED_FRAMES_X, ex_video_tag};

/// What one frame's SEI carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameCaptions {
    pub cea608: bool,
    pub cea708: bool,
    /// Any caption byte that is not padding: text is being sent.
    pub text: bool,
}

impl FrameCaptions {
    pub fn any(&self) -> bool {
        self.cea608 || self.cea708
    }

    fn merge(&mut self, other: FrameCaptions) {
        self.cea608 |= other.cea608;
        self.cea708 |= other.cea708;
        self.text |= other.text;
    }
}

/// SEI payload type of registered user data (ITU-T T.35).
const SEI_USER_DATA_REGISTERED: usize = 4;
/// T.35 country code (USA), provider code (ATSC) and A/53 identifier, then
/// `user_data_type_code` 3 (cc_data).
const A53_PREFIX: &[u8] = &[0xB5, 0x00, 0x31, b'G', b'A', b'9', b'4', 0x03];

/// The captions in one video packet. `hevc_hint` names the codec of a raw
/// (Annex B) payload; FLV payloads say it themselves.
pub fn frame_captions(format: PayloadFormat, payload: &[u8], hevc_hint: bool) -> FrameCaptions {
    match format {
        PayloadFormat::Raw => annexb_captions(payload, hevc_hint),
        PayloadFormat::Flv => {
            if let Some(tag) = ex_video_tag(payload) {
                return match tag.packet_type {
                    EX_CODED_FRAMES | EX_CODED_FRAMES_X => {
                        length_prefixed_captions(tag.body, tag.is_hevc())
                    }
                    _ => FrameCaptions::default(),
                };
            }
            // Legacy FLV: codec id 7 (H.264) or 12 (H.265), packet type 1
            // (NAL units) after a 24-bit composition time.
            match payload {
                [tag, 1, _, _, _, body @ ..] if matches!(tag & 0x0F, 7 | 12) => {
                    length_prefixed_captions(body, tag & 0x0F == 12)
                }
                _ => FrameCaptions::default(),
            }
        }
    }
}

/// What a NAL unit is, for the walk.
enum Nal {
    Sei,
    Picture,
    Other,
}

fn classify(nal: &[u8], hevc: bool) -> Nal {
    let Some(&first) = nal.first() else {
        return Nal::Other;
    };
    if hevc {
        match (first >> 1) & 0x3F {
            39 => Nal::Sei,
            0..=31 => Nal::Picture,
            _ => Nal::Other,
        }
    } else {
        match first & 0x1F {
            6 => Nal::Sei,
            1..=5 => Nal::Picture,
            _ => Nal::Other,
        }
    }
}

/// Four-byte NAL lengths, as every H.264 and H.265 FLV encoder writes.
fn length_prefixed_captions(mut body: &[u8], hevc: bool) -> FrameCaptions {
    let mut found = FrameCaptions::default();
    while let Some((&len, rest)) = body.split_first_chunk::<4>() {
        let len = u32::from_be_bytes(len) as usize;
        let Some(nal) = rest.get(..len) else {
            break;
        };
        match classify(nal, hevc) {
            Nal::Picture => break,
            Nal::Sei => found.merge(sei_captions(nal, hevc)),
            Nal::Other => {}
        }
        body = &rest[len..];
    }
    found
}

fn annexb_captions(data: &[u8], hevc: bool) -> FrameCaptions {
    let mut found = FrameCaptions::default();
    let finder = memmem::Finder::new(&[0u8, 0, 1]);
    let mut starts = finder.find_iter(data).map(|at| at + 3).peekable();
    while let Some(start) = starts.next() {
        let Some(nal_head) = data.get(start..) else {
            break;
        };
        match classify(nal_head, hevc) {
            Nal::Picture => break,
            Nal::Sei => {
                // The NAL ends where the next start code begins (its
                // leading zero byte, if any, is trailing data).
                let end = starts.peek().map_or(data.len(), |next| next - 3);
                if let Some(nal) = data.get(start..end) {
                    found.merge(sei_captions(nal, hevc));
                }
            }
            Nal::Other => {}
        }
    }
    found
}

/// The SEI messages of one SEI NAL unit (header included).
fn sei_captions(nal: &[u8], hevc: bool) -> FrameCaptions {
    let header = if hevc { 2 } else { 1 };
    let Some(payload) = nal.get(header..) else {
        return FrameCaptions::default();
    };
    let rbsp = rbsp(payload);
    let mut found = FrameCaptions::default();
    let mut at = 0usize;
    // Each message: ff-extended type and size, then the payload; the RBSP
    // ends with its stop bit (0x80).
    while at + 1 < rbsp.len() {
        let Some((kind, next)) = ff_coded(&rbsp, at) else {
            break;
        };
        let Some((size, next)) = ff_coded(&rbsp, next) else {
            break;
        };
        let Some(message) = rbsp.get(next..next.saturating_add(size)) else {
            break;
        };
        if kind == SEI_USER_DATA_REGISTERED {
            found.merge(a53_captions(message));
        }
        at = next + size;
    }
    found
}

/// An SEI type or size: 0xFF bytes each add 255, the last byte ends it.
fn ff_coded(data: &[u8], mut at: usize) -> Option<(usize, usize)> {
    let mut value = 0usize;
    loop {
        let byte = *data.get(at)?;
        at += 1;
        value = value.checked_add(usize::from(byte))?;
        if byte != 0xFF {
            return Some((value, at));
        }
    }
}

/// A/53 cc_data: a flags byte (`process_cc_data_flag`, `cc_count`), a
/// reserved byte, then `cc_count` triples of (marker, valid, type) and two
/// data bytes. Types 0 and 1 are CEA-608 fields, 2 and 3 CEA-708 DTVCC.
fn a53_captions(message: &[u8]) -> FrameCaptions {
    let Some(rest) = message.strip_prefix(A53_PREFIX) else {
        return FrameCaptions::default();
    };
    let [flags, _reserved, triples @ ..] = rest else {
        return FrameCaptions::default();
    };
    let mut found = FrameCaptions::default();
    if flags & 0x40 == 0 {
        return found;
    }
    let count = usize::from(flags & 0x1F);
    for triple in triples.chunks_exact(3).take(count) {
        let (head, one, two) = (triple[0], triple[1], triple[2]);
        if head & 0x04 == 0 {
            continue;
        }
        match head & 0x03 {
            // Odd parity in bit 7: the character is the low seven bits.
            0 | 1 => {
                found.cea608 = true;
                found.text |= one & 0x7F != 0 || two & 0x7F != 0;
            }
            _ => {
                found.cea708 = true;
                found.text |= one != 0 || two != 0;
            }
        }
    }
    found
}

#[cfg(test)]
pub(crate) mod test_data;
#[cfg(test)]
mod tests;
