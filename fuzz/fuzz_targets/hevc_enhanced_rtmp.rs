//! Enhanced RTMP HEVC from untrusted Annex-B (SRT ingest): the hvcC
//! sequence-header builder (SPS profile/tier/level parser) and the coded
//! frame packer.
#![no_main]

use restream::media::codec::{
    build_hevc_enhanced_rtmp_sequence_header, hevc_video_for_enhanced_rtmp_with_composition_into,
};

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    if let Some(header) = build_hevc_enhanced_rtmp_sequence_header(data) {
        assert_eq!(&header[..5], b"\x80hvc1");
        assert_eq!(header[5 + 22], 3, "VPS, SPS and PPS arrays");
    }
    let composition = data
        .get(..3)
        .map_or(0, |b| (i32::from(b[0]) << 16 | i32::from(b[1]) << 8 | i32::from(b[2])) << 8 >> 8);
    let mut out = Vec::new();
    let _ = hevc_video_for_enhanced_rtmp_with_composition_into(data, true, composition, &mut out);
    assert_eq!(&out[1..5], b"hvc1");
});
