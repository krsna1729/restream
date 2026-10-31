//! Caption test data: SEI messages a camera or encoder would send. Builds
//! input only; production reads captions and rewrites nothing.

use super::{A53_PREFIX, SEI_USER_DATA_REGISTERED};

/// One A/53 cc_data SEI message: `triples` of (cc_type, byte, byte), all
/// valid.
pub(crate) fn a53(triples: &[(u8, u8, u8)]) -> Vec<u8> {
    let mut message = A53_PREFIX.to_vec();
    message.push(0x40 | triples.len() as u8);
    message.push(0xFF);
    for &(kind, one, two) in triples {
        message.extend_from_slice(&[0xFC | kind, one, two]);
    }
    message.push(0xFF);
    message
}

/// An SEI NAL (H.264 or H.265 header) holding one registered user data
/// message, with emulation prevention applied.
pub(crate) fn sei_nal(message: &[u8], hevc: bool) -> Vec<u8> {
    let mut payload = vec![SEI_USER_DATA_REGISTERED as u8, message.len() as u8];
    payload.extend_from_slice(message);
    payload.push(0x80);
    let mut nal = if hevc { vec![39 << 1, 1] } else { vec![0x06] };
    let mut zeros = 0;
    for byte in payload {
        if zeros >= 2 && byte <= 3 {
            nal.push(3);
            zeros = 0;
        }
        nal.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
    }
    nal
}
