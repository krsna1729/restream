use bytes::Bytes;
use moq_net::track;

use super::*;
use crate::media::codec::build_avcc_sequence_header;
use crate::media::egress::journal::FeedEpoch;
use crate::media::moq::egress::startup::{MoqPublishStartup, MoqVideoStartup};
use crate::media::moq::media::{CATALOG_TRACK, VideoCodec};
use crate::media::moq::transport::{self, test_support::run};
use crate::media::packet::PayloadFormat;
use crate::media::ring_buffer::RingBuffer;

const PARAMETER_SETS: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2,
];

/// A broadcast on its own budgeted origin, as the leaf and the origin make
/// one; the origin is returned so it outlives the test's reads.
fn broadcast(name: &str, video: bool, audio: bool) -> (origin::Producer, HangBroadcast) {
    let (origin, driver, budget) = crate::media::moq::budget::origin();
    compio::runtime::spawn(transport::run_driver(driver)).detach();
    let broadcast = HangBroadcast::create(&origin, budget, name, video, audio).unwrap();
    (origin, broadcast)
}

fn frame(track: TrackKind, keyframe: bool, pts_us: u64) -> MediaFrame {
    MediaFrame {
        track,
        keyframe,
        pts_us,
        data: LocBuffer::from_payload(&pts_us.to_be_bytes()),
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
        out.push(parse_loc_frame(frame.payload).unwrap().0);
    }
    out
}

/// The next frame of `group`, a LOC end marker: its timestamp.
async fn marker(group: &mut group::Consumer) -> u64 {
    let frame = group.read_frame().await.unwrap().expect("a marker");
    assert_eq!(
        moq_loc::decode(frame.payload.clone()).unwrap().timescale,
        Some(1_000_000),
        "duration markers use the same media epoch as access units"
    );
    let (us, payload) = parse_loc_frame(frame.payload).unwrap();
    assert!(payload.is_empty(), "a marker is empty");
    us
}

#[test]
fn video_groups_are_gops_and_frames_before_the_first_keyframe_are_dropped() {
    run(async {
        let (_origin, mut broadcast) = broadcast("b", true, false);
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
        assert_eq!(marker(&mut gop).await, 166, "the GOP's end marker");
        assert!(
            gop.read_frame().await.unwrap().is_none(),
            "a keyframe ends the GOP"
        );
        let mut next = video.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut next, 1).await, [166]);
    });
}

#[test]
fn audio_groups_span_about_a_second() {
    run(async {
        let (_origin, mut broadcast) = broadcast("b", false, true);
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
    });
}

/// The timestamp of the next media frame on `track`, past any marker.
async fn next_media(track: &mut track::Subscriber) -> u64 {
    loop {
        let mut group = track.recv_group().await.unwrap().expect("a group");
        let frame = group.read_frame().await.unwrap().expect("a frame");
        let (us, payload) = parse_loc_frame(frame.payload).unwrap();
        if !payload.is_empty() {
            return us;
        }
    }
}

#[test]
fn a_rewind_moves_both_tracks_on_by_one_offset() {
    run(async {
        let (_origin, mut broadcast) = broadcast("b", true, true);
        let mut video = subscribe(&broadcast, VIDEO_TRACK).await;
        let mut audio = subscribe(&broadcast, AUDIO_TRACK).await;
        for (track, keyframe, pts) in [
            (TrackKind::Video, true, 10_000_000),
            (TrackKind::Audio, true, 10_000_000),
            (TrackKind::Video, false, 10_033_000),
            (TrackKind::Audio, true, 10_021_000),
        ] {
            assert_eq!(broadcast.write(frame(track, keyframe, pts)), Ok(true));
        }
        let mut gop = video.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut gop, 2).await, [10_000_000, 10_033_000]);
        let mut sound = audio.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut sound, 2).await, [10_000_000, 10_021_000]);

        // A new ingest session: the ring starts again at zero.
        assert_eq!(broadcast.write(frame(TrackKind::Video, true, 0)), Ok(true));
        assert_eq!(
            broadcast.write(frame(TrackKind::Audio, true, 21_000)),
            Ok(true)
        );
        // The old GOP ends one frame after its last; the audio group ends
        // at the discontinuity instead of taking the next frame.
        assert_eq!(marker(&mut gop).await, 10_066_000);
        assert!(gop.read_frame().await.unwrap().is_none());
        assert!(sound.read_frame().await.unwrap().is_none());
        // Both resume just past the furthest point presented (video, one
        // step on), shifted alike: still 21 ms apart.
        assert_eq!(next_media(&mut video).await, 10_066_000);
        assert_eq!(next_media(&mut audio).await, 10_087_000);
    });
}

#[test]
fn reordered_video_stays_in_its_gop_without_reanchoring_audio() {
    run(async {
        let (_origin, mut broadcast) = broadcast("b", true, true);
        let mut video = subscribe(&broadcast, VIDEO_TRACK).await;
        let mut audio = subscribe(&broadcast, AUDIO_TRACK).await;
        for (track, keyframe, pts) in [
            (TrackKind::Video, true, 1_000_000),
            (TrackKind::Video, false, 1_066_000),
            (TrackKind::Video, false, 1_033_000),
            (TrackKind::Audio, true, 1_021_000),
        ] {
            assert_eq!(broadcast.write(frame(track, keyframe, pts)), Ok(true));
        }
        let mut gop = video.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut gop, 3).await, [1_000_000, 1_066_000, 1_033_000]);
        assert_eq!(next_media(&mut audio).await, 1_021_000);
        assert_eq!(broadcast.timeline.offset_us, 0);
    });
}

#[test]
fn loc_subscribers_share_the_converters_payload_allocation() {
    run(async {
        let (_origin, mut broadcast) = broadcast("b", true, false);
        let mut first = subscribe(&broadcast, VIDEO_TRACK).await;
        let mut second = subscribe(&broadcast, VIDEO_TRACK).await;
        let data = LocBuffer::from_payload(b"encoded access unit");
        let payload_at = data.payload().as_ptr();
        assert_eq!(
            broadcast.write(MediaFrame {
                track: TrackKind::Video,
                keyframe: true,
                pts_us: 42_000,
                data,
            }),
            Ok(true)
        );
        for subscriber in [&mut first, &mut second] {
            let mut group = subscriber.recv_group().await.unwrap().unwrap();
            let frame = group.read_frame().await.unwrap().unwrap();
            let (timestamp, payload) = parse_loc_frame(frame.payload).unwrap();
            assert_eq!(timestamp, 42_000);
            assert_eq!(payload.as_ref(), b"encoded access unit");
            assert_eq!(payload.as_ptr(), payload_at);
        }
    });
}

#[test]
fn audio_can_reanchor_both_tracks_before_the_next_video_keyframe() {
    run(async {
        let (_origin, mut broadcast) = broadcast("b", true, true);
        let mut video = subscribe(&broadcast, VIDEO_TRACK).await;
        let mut audio = subscribe(&broadcast, AUDIO_TRACK).await;
        for (track, keyframe, pts) in [
            (TrackKind::Video, true, 10_000_000),
            (TrackKind::Video, false, 10_033_000),
            (TrackKind::Audio, true, 10_000_000),
            (TrackKind::Audio, true, 10_021_000),
        ] {
            assert_eq!(broadcast.write(frame(track, keyframe, pts)), Ok(true));
        }
        let mut gop = video.recv_group().await.unwrap().unwrap();
        let mut sound = audio.recv_group().await.unwrap().unwrap();
        assert_eq!(read(&mut gop, 2).await, [10_000_000, 10_033_000]);
        assert_eq!(read(&mut sound, 2).await, [10_000_000, 10_021_000]);

        assert_eq!(broadcast.write(frame(TrackKind::Audio, true, 0)), Ok(true));
        assert_eq!(
            broadcast.write(frame(TrackKind::Video, false, 12_000)),
            Ok(false),
            "video must reacquire a keyframe after the shared discontinuity"
        );
        assert_eq!(
            broadcast.write(frame(TrackKind::Video, true, 33_000)),
            Ok(true)
        );
        assert!(gop.read_frame().await.unwrap().is_none());
        assert!(sound.read_frame().await.unwrap().is_none());
        assert_eq!(next_media(&mut audio).await, 10_066_000);
        assert_eq!(next_media(&mut video).await, 10_099_000);
    });
}

#[test]
fn a_dropped_broadcast_ends_every_track_cleanly_after_its_media() {
    run(async {
        let (_origin, mut broadcast) = broadcast("b", true, false);
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
        let (_origin, broadcast) = broadcast("b", true, false);
        let startup = MoqPublishStartup {
            video: Some(MoqVideoStartup {
                codec: VideoCodec::H264,
                config: None,
            }),
            ..MoqPublishStartup::default()
        };

        let mut catalog = subscribe(&broadcast, CATALOG_TRACK).await;
        let mut video = subscribe(&broadcast, VIDEO_TRACK).await;
        let mut msf = subscribe(&broadcast, "catalog").await;
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
        // 40 ms: the 6-byte body (moq-net sees the 6-byte LOC header too).
        assert_eq!(
            publisher.pump(&feed, &mut cursor, budget),
            Pump::Sent { bytes: 6, units: 2 }
        );
        assert_eq!(publisher.pump(&feed, &mut cursor, budget), Pump::Empty);
        let first = next_catalog(&mut catalog).await;
        assert_eq!(
            first.video.renditions[VIDEO_TRACK].description,
            Some(header.slice(5..))
        );
        // The MSF catalog describes the same track, packaged as LOC.
        let mut group = msf.recv_group().await.unwrap().unwrap();
        let json = group.read_frame().await.unwrap().unwrap().payload;
        let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
        let tracks = json["tracks"].as_array().expect("MSF tracks");
        assert_eq!(tracks.len(), 1, "{json}");
        assert_eq!(tracks[0]["name"], VIDEO_TRACK, "{json}");
        assert_eq!(tracks[0]["packaging"], "loc", "{json}");
        let mut gop = video.recv_group().await.unwrap().unwrap();
        let sent = gop.read_frame().await.unwrap().unwrap();
        assert_eq!(
            parse_loc_frame(sent.payload),
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
        let startup = MoqPublishStartup::default();
        let (_origin, broadcast) = broadcast("b", false, false);
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

/// A `size`-byte frame at `pts_us`.
fn sized(track: TrackKind, keyframe: bool, pts_us: u64, size: usize) -> MediaFrame {
    MediaFrame {
        track,
        keyframe,
        pts_us,
        data: LocBuffer::from_payload(&vec![0xAB; size]),
    }
}

#[test]
fn sustained_writes_keep_the_origin_cache_within_about_two_gops() {
    run(async {
        let (_origin, mut broadcast) = broadcast("b", true, true);
        // 2 s GOPs at 30 fps (60 kB keyframe, 12 kB deltas) and AAC
        // (370 B every 21.33 ms): 16 s at a time would be 6 MB.
        const GOPS: u64 = 40;
        let gop_bytes = (60_000 + 59 * 12_000 + 94 * 370) as u64;
        for gop in 0..GOPS {
            let start = gop * 2_000_000;
            for index in 0..60 {
                let size = if index == 0 { 60_000 } else { 12_000 };
                let pts = start + index * 1_000_000 / 30;
                assert_eq!(
                    broadcast.write(sized(TrackKind::Video, index == 0, pts, size)),
                    Ok(true)
                );
            }
            for index in 0..94 {
                let pts = start + index * 1024 * 1_000_000 / 48_000;
                assert_eq!(
                    broadcast.write(sized(TrackKind::Audio, true, pts, 370)),
                    Ok(true)
                );
            }
            let used = broadcast.budget.used();
            assert!(
                used <= 3 * gop_bytes,
                "after GOP {gop} the cache holds {used} bytes, over three GOPs ({})",
                3 * gop_bytes
            );
        }
    });
}
