//! Bytes from an RTMP publisher after the handshake, as the ingest
//! `ServerSession` reads them (connect, publish, metadata, media). The first
//! byte of each read gives its length, so read boundaries are fuzzed too.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let mut reads = Vec::new();
    let mut rest = data;
    while let Some((&len, tail)) = rest.split_first() {
        let len = usize::from(len).max(1).min(tail.len());
        let (read, next) = tail.split_at(len);
        reads.push(read);
        rest = next;
    }
    restream::media::rtmp::fuzz_entry::rtmp_client_requests(reads);
});
