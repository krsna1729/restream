use bytes::{Bytes, BytesMut};

use super::*;
use crate::media::codec::build_avcc_sequence_header;
use crate::media::egress::journal::FeedEpoch;
use crate::media::moq::egress::startup::{MoqPublishStartup, MoqVideoStartup};
use crate::media::moq::media::{
    CATALOG_TRACK, VideoCodec, parse_legacy_frame, write_legacy_prefix,
};
use crate::media::moq::transport::{self, test_support::run};
use crate::media::packet::PayloadFormat;
use crate::media::ring_buffer::RingBuffer;

const PARAMETER_SETS: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2,
];

fn new_origin() -> origin::Producer {
    let (origin, driver) = origin::Producer::new(origin::Config::new(moq_net::Hop::random()));
    compio::runtime::spawn(transport::run_driver(driver)).detach();
    origin
}

fn frame(track: TrackKind, keyframe: bool, pts_us: u64) -> HangFrame {
    let mut data = BytesMut::new();
    write_legacy_prefix(pts_us, &mut data).unwrap();
    data.extend_from_slice(&pts_us.to_be_bytes());
    HangFrame {
        track,
        keyframe,
        pts_us,
        data: data.freeze(),
    }
}

async fn subscribe(broadcast: &HangBroadcast, track: &str) -> track::Subscriber {
    broadcast
        .broadcast
        .consume()
        .track(track)
        .unwrap()
        .subscribe(None)
        .await
        .unwrap()
}

/// The timestamps of the next `count` frames of `group`.
async fn read(group: &mut group::Consumer, count: usize) -> Vec<u64> {
    let mut out = Vec::new();
    for _ in 0..count {
        let frame = group.read_frame().await.unwrap().expect("a frame");
        out.push(parse_legacy_frame(frame.payload).unwrap().0);
    }
    out
}

#[test]
fn video_groups_are_gops_and_frames_before_the_first_keyframe_are_dropped() {
    run(async {
        let origin = new_origin();
        let mut broadcast = HangBroadcast::create(&origin, "b", true, false).unwrap();
        let mut video = subscribe(&broadcast, VIDEO_TRACK).await;

        assert_eq!(
            broadcast.write(frame(TrackKind::Video, false, 1)),
            Ok(false)
        );
        assert_eq!(
            broadcast.write(frame(TrackKind::Audio, true, 1)),
            Ok(false),
            "no audio track"
        );
        assert_eq!(
            broadcast.write(frame(TrackKind::Video, true, 100)),
            Ok(true)
        );
        assert_eq!(
            broadcast.write(frame(TrackKind::Video, false, 133)),
            Ok(true)
        );
        let mut gop = video.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut gop, 2).await, [100, 133]);

        assert_eq!(
            broadcast.write(frame(TrackKind::Video, true, 166)),
            Ok(true)
        );
        assert!(
            gop.read_frame().await.unwrap().is_none(),
            "a keyframe ends the GOP"
        );
        let mut next = video.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut next, 1).await, [166]);
    });
}

#[test]
fn audio_groups_span_about_a_second_and_restart_on_a_backwards_jump() {
    run(async {
        let origin = new_origin();
        let mut broadcast = HangBroadcast::create(&origin, "b", false, true).unwrap();
        let mut audio = subscribe(&broadcast, AUDIO_TRACK).await;

        for pts in [5_000_000, 5_500_000, 5_999_999] {
            assert_eq!(
                broadcast.write(frame(TrackKind::Audio, true, pts)),
                Ok(true)
            );
        }
        let mut first = audio.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut first, 3).await, [5_000_000, 5_500_000, 5_999_999]);

        broadcast
            .write(frame(TrackKind::Audio, true, 6_000_000))
            .unwrap();
        assert!(first.read_frame().await.unwrap().is_none());
        let mut second = audio.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut second, 1).await, [6_000_000]);

        broadcast.write(frame(TrackKind::Audio, true, 10)).unwrap();
        assert!(second.read_frame().await.unwrap().is_none());
        let mut third = audio.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut third, 1).await, [10]);
    });
}

#[test]
fn a_dropped_broadcast_ends_every_track_cleanly_after_its_media() {
    run(async {
        let origin = new_origin();
        let mut broadcast = HangBroadcast::create(&origin, "b", true, false).unwrap();
        let mut video = subscribe(&broadcast, VIDEO_TRACK).await;
        let mut catalog = subscribe(&broadcast, CATALOG_TRACK).await;
        assert_eq!(
            broadcast.write(frame(TrackKind::Video, true, 100)),
            Ok(true)
        );
        // As when the origin's last viewer leaves: the pump is cancelled
        // and the broadcast dropped, never finished by hand.
        drop(broadcast);

        let mut gop = video.recv_group().await.unwrap().expect("the written GOP");
        assert_eq!(read(&mut gop, 1).await, [100]);
        assert!(gop.read_frame().await.unwrap().is_none(), "the GOP ends");
        assert!(
            video.recv_group().await.unwrap().is_none(),
            "the video track ends cleanly"
        );
        assert!(
            catalog.recv_group().await.unwrap().is_none(),
            "the catalog track ends cleanly"
        );
    });
}

fn flv(keyframe: bool, pts: i64, payload: Vec<u8>) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts,
        dts: pts,
        is_keyframe: keyframe,
        format: PayloadFormat::Flv,
        payload: Bytes::from(payload),
    }
}

async fn next_catalog(catalog: &mut track::Subscriber) -> hang::Catalog {
    let mut group = catalog.recv_group().await.unwrap().unwrap();
    let frame = group.read_frame().await.unwrap().unwrap();
    hang::Catalog::from_slice(&frame.payload).unwrap()
}

#[test]
fn the_ring_publisher_publishes_the_catalog_first_and_again_on_a_new_record() {
    run(async {
        let origin = new_origin();
        let startup = MoqPublishStartup {
            video: Some(MoqVideoStartup {
                codec: VideoCodec::H264,
                config: None,
            }),
            ..MoqPublishStartup::default()
        };
        let broadcast = HangBroadcast::create(&origin, "b", true, false).unwrap();
        let mut catalog = subscribe(&broadcast, CATALOG_TRACK).await;
        let mut video = subscribe(&broadcast, VIDEO_TRACK).await;
        let mut publisher = RingPublisher::new(MediaConverter::new(&startup), broadcast);
        let ring = Arc::new(RingBuffer::new(64));
        let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
        let mut cursor = FeedCursor::new(feed.epoch(), 0);
        let budget = ReadBudget::new(16, 1 << 20);

        // A delta frame before any configuration: consumed, not sent.
        ring.push(flv(false, 0, vec![0x27, 1, 0, 0, 0, 0, 0, 0, 1, 0x41]));
        assert_eq!(
            publisher.pump(&feed, &mut cursor, budget),
            Pump::Sent { bytes: 0, units: 1 }
        );

        let header = build_avcc_sequence_header(PARAMETER_SETS).unwrap();
        ring.push(flv(false, 0, header.to_vec()));
        let idr = vec![0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 0x88];
        ring.push(flv(true, 40, idr.clone()));
        // 40 ms = 40 000 µs: a 4-byte prefix, then the 6-byte body.
        assert_eq!(
            publisher.pump(&feed, &mut cursor, budget),
            Pump::Sent {
                bytes: 10,
                units: 2
            }
        );
        assert_eq!(publisher.pump(&feed, &mut cursor, budget), Pump::Empty);
        let first = next_catalog(&mut catalog).await;
        assert_eq!(
            first.video.renditions[VIDEO_TRACK].description,
            Some(header.slice(5..))
        );
        let mut gop = video.recv_group().await.unwrap().unwrap();
        let sent = gop.read_frame().await.unwrap().unwrap();
        assert_eq!(
            parse_legacy_frame(sent.payload),
            Ok((40_000, Bytes::from(idr[5..].to_vec())))
        );

        // A new record: the catalog is republished before the next frame.
        let mut other = PARAMETER_SETS.to_vec();
        *other.last_mut().unwrap() ^= 1;
        let header = build_avcc_sequence_header(&other).unwrap();
        ring.push(flv(false, 80, header.to_vec()));
        ring.push(flv(true, 80, idr));
        assert!(matches!(
            publisher.pump(&feed, &mut cursor, budget),
            Pump::Sent { units: 2, .. }
        ));
        let second = next_catalog(&mut catalog).await;
        assert_eq!(
            second.video.renditions[VIDEO_TRACK].description,
            Some(header.slice(5..))
        );
        assert_eq!(publisher.skipped_units(), 0);
    });
}

#[test]
fn a_cursor_the_ring_overwrote_is_an_overrun_and_live_start_is_a_keyframe() {
    run(async {
        let origin = new_origin();
        let startup = MoqPublishStartup::default();
        let broadcast = HangBroadcast::create(&origin, "b", false, false).unwrap();
        let mut publisher = RingPublisher::new(MediaConverter::new(&startup), broadcast);
        let ring = Arc::new(RingBuffer::new(4));
        let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
        for index in 0..10 {
            ring.push(flv(index == 7, index, vec![0x17, 1, 0, 0, 0]));
        }
        let mut cursor = FeedCursor::new(feed.epoch(), 0);
        let budget = ReadBudget::new(16, 1 << 20);
        assert_eq!(publisher.pump(&feed, &mut cursor, budget), Pump::Overrun);
        assert_eq!(
            cursor.next_sequence, 0,
            "an overrun leaves the cursor for the caller"
        );
        assert_eq!(live_start(&feed).next_sequence, 7);
    });
}
