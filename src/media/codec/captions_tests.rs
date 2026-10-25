use super::*;

/// One A/53 cc_data SEI message: `triples` of (cc_type, byte, byte), all
/// valid.
fn a53(triples: &[(u8, u8, u8)]) -> Vec<u8> {
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
fn sei_nal(message: &[u8], hevc: bool) -> Vec<u8> {
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

fn slice(hevc: bool) -> Vec<u8> {
    if hevc {
        vec![1 << 1, 1, 0xAF, 0x00, 0x00, 0x00, 0x01]
    } else {
        vec![0x65, 0x88, 0x84]
    }
}

fn annexb(nals: &[Vec<u8>]) -> Vec<u8> {
    nals.iter()
        .flat_map(|nal| [&[0, 0, 0, 1][..], nal].concat())
        .collect()
}

fn length_prefixed(nals: &[Vec<u8>]) -> Vec<u8> {
    nals.iter()
        .flat_map(|nal| [&(nal.len() as u32).to_be_bytes()[..], nal].concat())
        .collect()
}

const HELLO: (u8, u8, u8) = (0, b'H' | 0x80, b'E');
const PADDING_608: (u8, u8, u8) = (0, 0x80, 0x80);

#[test]
fn cea608_text_in_h264_annexb_and_both_flv_forms() {
    let nals = [sei_nal(&a53(&[HELLO]), false), slice(false)];
    let expected = FrameCaptions {
        cea608: true,
        cea708: false,
        text: true,
    };
    assert_eq!(
        frame_captions(PayloadFormat::Raw, &annexb(&nals), false),
        expected
    );

    let legacy = [&[0x17, 1, 0, 0, 0][..], &length_prefixed(&nals)].concat();
    assert_eq!(frame_captions(PayloadFormat::Flv, &legacy, false), expected);

    let enhanced = [
        &[0x80 | 0x10 | 3, b'a', b'v', b'c', b'1'][..],
        &length_prefixed(&nals),
    ]
    .concat();
    assert_eq!(
        frame_captions(PayloadFormat::Flv, &enhanced, false),
        expected
    );
}

#[test]
fn h265_prefix_sei_carries_708_too() {
    let nals = [
        sei_nal(&a53(&[(2, 0x03, 0x41), PADDING_608]), true),
        slice(true),
    ];
    let found = frame_captions(PayloadFormat::Raw, &annexb(&nals), true);
    assert_eq!(
        found,
        FrameCaptions {
            cea608: true,
            cea708: true,
            text: true
        }
    );
    let enhanced = [
        &[0x80 | 0x10 | 3, b'h', b'v', b'c', b'1'][..],
        &length_prefixed(&nals),
    ]
    .concat();
    assert_eq!(frame_captions(PayloadFormat::Flv, &enhanced, false), found);
}

#[test]
fn padding_is_captions_present_without_text() {
    let nals = [
        sei_nal(&a53(&[PADDING_608, PADDING_608]), false),
        slice(false),
    ];
    assert_eq!(
        frame_captions(PayloadFormat::Raw, &annexb(&nals), false),
        FrameCaptions {
            cea608: true,
            cea708: false,
            text: false
        }
    );
}

#[test]
fn other_user_data_invalid_triples_and_sei_after_the_picture_say_nothing() {
    let mut not_atsc = a53(&[HELLO]);
    not_atsc[0] = 0x26;
    let invalid = {
        let mut message = a53(&[HELLO]);
        let at = A53_PREFIX.len() + 2;
        message[at] &= !0x04;
        message
    };
    let not_processed = {
        let mut message = a53(&[HELLO]);
        message[A53_PREFIX.len()] &= !0x40;
        message
    };
    for message in [not_atsc, invalid, not_processed] {
        let nals = [sei_nal(&message, false), slice(false)];
        assert_eq!(
            frame_captions(PayloadFormat::Raw, &annexb(&nals), false),
            FrameCaptions::default()
        );
    }
    let late = [slice(false), sei_nal(&a53(&[HELLO]), false)];
    assert_eq!(
        frame_captions(PayloadFormat::Raw, &annexb(&late), false),
        FrameCaptions::default(),
        "the walk stops at the picture"
    );
    let late_flv = [&[0x17, 1, 0, 0, 0][..], &length_prefixed(&late)].concat();
    assert_eq!(
        frame_captions(PayloadFormat::Flv, &late_flv, false),
        FrameCaptions::default(),
        "the FLV walk stops at the picture too"
    );
}

#[test]
fn a_sequence_header_and_truncated_input_are_not_frames() {
    let header = [0x17, 0, 0, 0, 0, 1, 0x64, 0, 0x1F];
    assert_eq!(
        frame_captions(PayloadFormat::Flv, &header, false),
        FrameCaptions::default()
    );
    let nals = [sei_nal(&a53(&[HELLO]), false), slice(false)];
    let whole = annexb(&nals);
    for cut in 0..whole.len() {
        let _ = frame_captions(PayloadFormat::Raw, &whole[..cut], false);
        let flv = [
            &[0x17, 1, 0, 0, 0][..],
            &length_prefixed(&nals)[..cut.min(length_prefixed(&nals).len())],
        ]
        .concat();
        let _ = frame_captions(PayloadFormat::Flv, &flv, false);
    }
}

proptest::proptest! {
    /// Arbitrary bytes never panic, in either format or codec.
    #[test]
    fn arbitrary_payloads_never_panic(data in proptest::collection::vec(proptest::num::u8::ANY, 0..512), hevc in proptest::bool::ANY) {
        let _ = frame_captions(PayloadFormat::Raw, &data, hevc);
        let _ = frame_captions(PayloadFormat::Flv, &data, hevc);
    }
}
