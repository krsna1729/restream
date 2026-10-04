//! SRT ingest: the MPEG-TS demuxer (PAT/PMT, PES reassembly, the stream
//! probe in `mpegts_probe`) on untrusted bytes, fed in fuzzer-chosen reads.
//! The first byte picks the mode: odd forces the 0x47 sync byte on every
//! 188-byte packet so the fuzzer reaches table and PES parsing; even feeds
//! the bytes raw to exercise sync search and resync.
#![no_main]

use restream::media::mpegts::TsDemuxer;

const TS_PACKET: usize = 188;

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    let Some((&mode, body)) = data.split_first() else {
        return;
    };
    let mut stream = body.to_vec();
    if mode & 1 == 1 {
        for packet in stream.chunks_mut(TS_PACKET) {
            packet[0] = 0x47;
        }
    }
    let read = usize::from(mode >> 1).max(1) * 7;
    let mut demuxer = TsDemuxer::new();
    let mut packets = Vec::new();
    for chunk in stream.chunks(read) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut packets);
        packets.clear();
        let _ = demuxer.take_probe();
    }
    demuxer.flush();
    demuxer.drain_into(&mut packets);
});
