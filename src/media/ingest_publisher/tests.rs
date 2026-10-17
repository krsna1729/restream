//! The shared publish path on the input it must ride out. File ingest reads
//! FFmpeg's stdout and propagates whatever `publish` returns, so a corrupt
//! stretch of that pipe must never fail the publisher: before the fMP4 work
//! `publish` returned `()`, and the TS demux still has no rejection to give.

use std::sync::Arc;

use super::*;
use crate::media::ring_buffer::RingBuffer;

/// The publication harness file ingest uses: an engine, a registered input
/// attempt, its ring and the target over them.
pub(super) async fn harness(name: &str) -> (Arc<MediaEngine>, PublishTarget, Arc<RingBuffer>) {
    let engine = Arc::new(MediaEngine::new());
    let ring = engine.get_or_create_pipeline(name).await;
    let registration = engine
        .try_register_ingest_attempt(name, name, "file")
        .await
        .expect("the ingest attempt registers");
    let target = PublishTarget::open(engine.clone(), name.to_string(), registration, ring.clone())
        .await
        .expect("the publish target opens");
    (engine, target, ring)
}

/// Deterministic bytes, so a failure is replayable.
fn pseudo_random(count: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

/// What the publisher put on the ring since `start`: video is Annex B, and
/// no track's packets may go backwards in time (the tracks interleave).
fn published_since(ring: &RingBuffer, start: usize) -> Vec<Arc<MediaPacket>> {
    let packets: Vec<Arc<MediaPacket>> = (start..ring.get_write_idx())
        .filter_map(|index| ring.read_at(index))
        .collect();
    for packet in &packets {
        if packet.media_type == MediaType::Video {
            assert!(
                packet.payload.starts_with(&[0, 0, 0, 1]),
                "a published video packet is not Annex B: {:02x?}",
                &packet.payload[..packet.payload.len().min(8)]
            );
        }
        assert!(!packet.payload.is_empty(), "an empty packet was published");
    }
    for media_type in [MediaType::Video, MediaType::Audio] {
        let track: Vec<i64> = packets
            .iter()
            .filter(|packet| packet.media_type == media_type)
            .map(|packet| packet.dts)
            .collect();
        assert!(
            track.windows(2).all(|pair| pair[0] <= pair[1]),
            "published {media_type:?} packets go backwards in time"
        );
    }
    packets
}

fn fixture() -> Vec<u8> {
    let path = crate::test_fixtures::canonical_h264_ts_fixture().expect("the TS fixture");
    std::fs::read(path).expect("the fixture reads")
}

/// Garbage of every shape file ingest can read off a broken pipe: the
/// publish path answers `Ok` for all of it, and nothing bogus is published.
#[tokio::test]
async fn garbage_never_fails_the_ts_publish_path() {
    let (_engine, target, ring) = harness("publish-garbage").await;
    let fixture = fixture();

    let mut truncated = fixture[..188 * 40].to_vec();
    truncated.truncate(truncated.len() - 100);
    let mut broken_header = vec![0x47u8; 188];
    broken_header[3] = 0xF0;

    // `true`: nothing in the case can be decoded, so nothing may be
    // published at all. `false`: real TS cut short, which may publish what
    // it can decode, but only well-formed packets.
    let cases: [(&str, Vec<u8>, bool); 6] = [
        ("random bytes", pseudo_random(4096, 1), true),
        (
            "random bytes with sync bytes",
            {
                let mut bytes = pseudo_random(4096, 2);
                for packet in bytes.chunks_mut(188) {
                    packet[0] = 0x47;
                }
                bytes
            },
            true,
        ),
        ("a truncated TS stretch", truncated, false),
        ("a bare sync byte", vec![0x47], true),
        ("an empty chunk", Vec::new(), true),
        ("a sync byte with a broken header", broken_header, true),
    ];
    for (name, bytes, undecodable) in cases {
        let case_start = ring.get_write_idx();
        for chunk in bytes.chunks(PUBLISH_CHUNK_BYTES) {
            let start = ring.get_write_idx();
            let mut publisher =
                SegmentPublisher::new(Container::Ts.demux(), IngestTimestamps::default());
            publisher
                .publish(&target, chunk)
                .await
                .unwrap_or_else(|error| panic!("{name}: publish refused the chunk: {error}"));
            publisher.flush(&target);
            published_since(&ring, start);
        }
        // Several chunks at once, and an object end, as file ingest ends a
        // read loop.
        let mut publisher =
            SegmentPublisher::new(Container::Ts.demux(), IngestTimestamps::default());
        for chunk in bytes.chunks(1024) {
            publisher
                .publish(&target, chunk)
                .await
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        publisher
            .end_object(&target)
            .unwrap_or_else(|error| panic!("{name}: end_object refused: {error}"));
        publisher.flush(&target);
        if undecodable {
            let published = published_since(&ring, case_start);
            assert!(
                published.is_empty(),
                "{name} published {} packets",
                published.len()
            );
        }
    }
}

/// The guarantee a reviewer asked for: a corrupt stretch of the pipe does
/// not stop the rest of the stream from publishing.
#[tokio::test]
async fn a_corrupt_stretch_still_publishes_the_rest() {
    let (_engine, target, ring) = harness("publish-corrupt-stretch").await;
    let fixture = fixture();
    let mut publisher = SegmentPublisher::new(Container::Ts.demux(), IngestTimestamps::default());
    let start = ring.get_write_idx();

    for chunk in pseudo_random(PUBLISH_CHUNK_BYTES, 3).chunks(4096) {
        publisher
            .publish(&target, chunk)
            .await
            .expect("garbage does not fail the publisher");
    }
    for chunk in fixture.chunks(PUBLISH_CHUNK_BYTES) {
        publisher
            .publish(&target, chunk)
            .await
            .expect("the fixture does not fail the publisher");
    }
    publisher.flush(&target);

    let packets = published_since(&ring, start);
    let video = packets
        .iter()
        .filter(|packet| packet.media_type == MediaType::Video)
        .count();
    assert!(
        video > 200,
        "only {video} video packets published after the corrupt stretch"
    );
    assert!(
        packets.iter().any(|packet| packet.is_keyframe),
        "no keyframe published after the corrupt stretch"
    );
}
