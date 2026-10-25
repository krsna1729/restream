//! Closed captions in video SEI (A/53 `GA94`): raw Annex B and FLV frames,
//! H.264 and H.265 NAL headers.
#![no_main]

use restream::media::codec::frame_captions;
use restream::media::packet::PayloadFormat;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let hevc = data.first().is_some_and(|byte| byte & 1 == 1);
    let _ = frame_captions(PayloadFormat::Raw, data, hevc);
    let _ = frame_captions(PayloadFormat::Flv, data, hevc);
});
