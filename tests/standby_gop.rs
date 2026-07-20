use bytes::Bytes;
use proptest::prelude::*;
use restream::media::packet::{MediaPacket, MediaType, PayloadFormat};
use restream::media::standby_gop::StandbyGopCache;

fn packet(media_type: MediaType, dts: i64, keyframe: bool, bytes: usize) -> MediaPacket {
    MediaPacket {
        media_type,
        format: PayloadFormat::Raw,
        is_keyframe: keyframe,
        track_index: 0,
        pts: dts,
        dts,
        payload: Bytes::from(vec![0; bytes]),
    }
}

#[test]
fn retains_only_the_latest_complete_gop() {
    let mut cache = StandbyGopCache::new(1_024, 16);
    cache.push(packet(MediaType::Audio, 0, false, 3));
    cache.push(packet(MediaType::Video, 10, true, 4));
    cache.push(packet(MediaType::Audio, 11, false, 3));
    cache.push(packet(MediaType::Video, 12, false, 5));
    cache.push(packet(MediaType::Video, 20, true, 6));
    cache.push(packet(MediaType::Audio, 21, false, 2));

    let replay = cache.take_replay();

    assert_eq!(
        replay.iter().map(|packet| packet.dts).collect::<Vec<_>>(),
        vec![20, 21]
    );
    assert!(!cache.is_replay_ready());
    assert!(cache.take_replay().is_empty());
}

#[test]
fn invalidates_an_oversized_gop_until_the_next_keyframe() {
    let mut cache = StandbyGopCache::new(8, 3);
    cache.push(packet(MediaType::Video, 10, true, 4));
    cache.push(packet(MediaType::Video, 11, false, 5));
    cache.push(packet(MediaType::Audio, 12, false, 1));
    assert!(!cache.is_replay_ready());

    cache.push(packet(MediaType::Video, 20, true, 4));
    cache.push(packet(MediaType::Audio, 21, false, 2));
    assert!(cache.is_replay_ready());
    assert_eq!(cache.packet_count(), 2);
    assert_eq!(cache.payload_bytes(), 6);
}

#[test]
fn packet_limit_invalidates_the_whole_gop() {
    let mut cache = StandbyGopCache::new(1_024, 2);
    cache.push(packet(MediaType::Video, 10, true, 1));
    cache.push(packet(MediaType::Audio, 11, false, 1));
    cache.push(packet(MediaType::Audio, 12, false, 1));

    assert!(!cache.is_replay_ready());
    assert!(cache.take_replay().is_empty());
}

proptest! {
    #[test]
    fn cache_never_exceeds_its_declared_limits(
        byte_limit in 1usize..4_096,
        packet_limit in 1usize..64,
        packets in prop::collection::vec(
            (any::<bool>(), any::<bool>(), 0usize..512),
            0..256,
        ),
    ) {
        let mut cache = StandbyGopCache::new(byte_limit, packet_limit);
        for (is_video, is_keyframe, payload_bytes) in packets {
            let media_type = if is_video {
                MediaType::Video
            } else {
                MediaType::Audio
            };
            cache.push(packet(media_type, 0, is_video && is_keyframe, payload_bytes));
            prop_assert!(cache.payload_bytes() <= byte_limit);
            prop_assert!(cache.packet_count() <= packet_limit);
            if cache.is_replay_ready() {
                let replay = cache.packets();
                prop_assert!(!replay.is_empty());
                prop_assert_eq!(replay[0].media_type, MediaType::Video);
                prop_assert!(replay[0].is_keyframe);
            }
        }
    }
}
