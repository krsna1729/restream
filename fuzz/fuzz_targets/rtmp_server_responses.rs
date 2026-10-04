//! Bytes from an RTMP destination server after the handshake, as read by
//! the egress client. The first byte of each read gives its length, so the
//! fuzzer also explores read boundaries.
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
    restream::media::rtmp::fuzz_entry::rtmp_server_responses(reads);
});
