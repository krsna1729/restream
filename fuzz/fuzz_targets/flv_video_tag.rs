//! RTMP ingest video tag probes: packet kind, composition offset,
//! AVCDecoderConfigurationRecord parameter sets, SPS metadata.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    restream::media::rtmp::fuzz_entry::flv_video_tag(data);
});
