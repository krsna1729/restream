use restream_mpegts::TsDemuxer;

/// A chunk that continues a partial packet is joined in a buffer the demuxer
/// keeps, not in a fresh chunk-sized allocation: on the MEDIA executor one
/// such feed is one poll, and allocate/copy/free of the whole chunk per poll
/// was measurable per-poll work (`hlsIngest`).
#[test]
fn continuing_a_partial_packet_reuses_its_staging_buffer() {
    // 100 bytes of a packet: no complete 188-byte packet, so it becomes the
    // partial tail the next feed must continue.
    let partial = [0x47; 100];
    // A chunk with no sync byte at all: nothing demuxes, nothing is emitted,
    // so allocations counted below can only come from the staging path.
    let body = vec![0xFFu8; 64 * 1024];
    let mut demuxer = TsDemuxer::new();
    let mut feed_pair = || {
        demuxer.feed(&partial);
        demuxer.feed(&body);
    };
    // Warm every buffer the path keeps.
    for _ in 0..3 {
        feed_pair();
    }

    crate::media::test_alloc::begin();
    for _ in 0..8 {
        feed_pair();
    }
    let allocations = crate::media::test_alloc::end();
    assert_eq!(
        allocations, 0,
        "{allocations} allocations in 8 partial-packet feeds; the staging buffer must be reused"
    );

    // Repeated non-packet-aligned feeds must preserve every ordered packet,
    // including the final PES emitted by flush and a pending tail on empty
    // feeds. Packet counts alone would miss reordered/corrupted payloads.
    let fixture =
        crate::media::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts_data = std::fs::read(&fixture).unwrap_or_else(|e| panic!("{e}"));
    let mut whole = TsDemuxer::new();
    whole.feed(&ts_data);
    whole.flush();
    let mut chunked = TsDemuxer::new();
    for chunk in ts_data.chunks(64 * 1024 + 7) {
        chunked.feed(chunk);
        chunked.feed(&[]);
    }
    chunked.flush();
    let packets = |mut demuxer: TsDemuxer| {
        demuxer
            .drain()
            .into_iter()
            .map(|packet| {
                (
                    packet.media_type,
                    packet.format,
                    packet.is_keyframe,
                    packet.track_index,
                    packet.pts,
                    packet.dts,
                    packet.payload,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(packets(whole), packets(chunked));
}
