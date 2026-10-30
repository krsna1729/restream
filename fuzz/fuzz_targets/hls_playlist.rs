//! HLS push ingest: the m3u8 parser on untrusted playlist bodies uploaded by
//! any stream key holder. The first byte picks the playlist path depth, so
//! relative URI resolution (`..`, nested variants) is explored too.
#![no_main]

use restream_engine::hls_ingest::playlist::parse;

const PATHS: [&str; 4] = ["index.m3u8", "v0/index.m3u8", "a/b/index.m3u8", "master.m3u8"];

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    let path = PATHS[usize::from(selector) % PATHS.len()];
    let _ = parse(body, path);
});
