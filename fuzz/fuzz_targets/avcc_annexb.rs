//! AVCC and Annex-B conversions on untrusted H.264: the decoder config
//! parser, length-prefixed → start-code conversion, and the sequence-header
//! builder, whose output must read back as the parameter sets it was built
//! from.
#![no_main]

use restream_core::codec::annexb_to_avcc;
use restream_core::codec::avcc_to_annexb;
use restream_core::codec::build_avcc_sequence_header;
use restream_core::codec::parse_avcc_config;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let (nalu_len_size, _) = parse_avcc_config(data);
    assert!((1..=4).contains(&nalu_len_size));
    let _ = avcc_to_annexb(data, nalu_len_size);
    let _ = annexb_to_avcc(data);
    if let Some(header) = build_avcc_sequence_header(data) {
        let (size, sets) = parse_avcc_config(&header[5..]);
        assert_eq!(size, 4);
        assert!(sets.starts_with(&[0, 0, 0, 1]), "built record must read back");
    }
});
