//! RTMP ingest audio tag probe, including the AAC AudioSpecificConfig.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    restream::media::rtmp::fuzz_entry::flv_audio_tag(data);
});
