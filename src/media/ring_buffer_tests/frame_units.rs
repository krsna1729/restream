use super::*;
use bytes::Bytes;
use proptest::prelude::*;

const SPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80];
const PPS_A: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2];
const PPS_B: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80];
const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88];
const P_SLICE: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 0x01];

fn packet(media_type: MediaType, track_index: u32, keyframe: bool, payload: &[u8]) -> MediaPacket {
    MediaPacket {
        media_type,
        format: PayloadFormat::Raw,
        is_keyframe: keyframe,
        track_index,
        pts: 0,
        dts: 0,
        payload: Bytes::copy_from_slice(payload),
    }
}

fn video(keyframe: bool, parts: &[&[u8]]) -> MediaPacket {
    packet(MediaType::Video, 0, keyframe, &parts.concat())
}

fn generations(ring: &RingBuffer) -> Vec<u32> {
    (0..ring.get_write_idx())
        .map(|sequence| {
            ring.frame_unit(sequence)
                .expect("retained")
                .config_generation
        })
        .collect()
}

#[test]
fn configuration_generation_changes_only_with_the_bytes_and_per_track() {
    let ring = RingBuffer::new(8);
    assert_eq!(ring.set_track_config(MediaType::Video, 0, vec![1, 2]), 1);
    assert_eq!(
        ring.set_track_config(MediaType::Video, 0, vec![1, 2]),
        1,
        "a repeated configuration keeps its generation"
    );
    assert_eq!(ring.set_track_config(MediaType::Video, 0, vec![1, 3]), 2);
    assert_eq!(
        ring.set_track_config(MediaType::Video, 0, vec![1, 2]),
        3,
        "returning to earlier bytes is still a change"
    );

    assert_eq!(ring.set_track_config(MediaType::Audio, 0, vec![1, 2]), 1);
    assert_eq!(ring.set_track_config(MediaType::Video, 1, vec![9]), 1);
    assert_eq!(
        ring.video_config().map(|config| config.generation),
        Some(3),
        "other tracks leave the video track alone"
    );
    assert_eq!(
        ring.set_track_config(MediaType::Video, 0, Vec::new()),
        3,
        "empty bytes are not a configuration"
    );

    let outside = CONFIG_TRACKS_PER_TYPE as u32;
    assert_eq!(ring.set_track_config(MediaType::Audio, outside, vec![1]), 0);
    assert!(ring.track_config(MediaType::Audio, outside).is_none());
}

#[test]
fn packets_carry_their_own_tracks_generation() {
    let ring = RingBuffer::new(8);
    ring.push(packet(MediaType::Video, 0, false, &[0]));
    ring.set_track_config(MediaType::Video, 0, vec![1]);
    ring.push(packet(MediaType::Video, 0, true, &[0]));
    ring.push(packet(MediaType::Audio, 0, false, &[0]));
    ring.set_track_config(MediaType::Audio, 1, vec![2]);
    ring.set_track_config(MediaType::Audio, 1, vec![3]);
    ring.push(packet(MediaType::Audio, 1, false, &[0]));
    ring.push(packet(MediaType::Video, 0, false, &[0]));

    assert_eq!(generations(&ring), [0, 1, 0, 2, 1]);
    assert_eq!(
        ring.track_config_at(3, MediaType::Audio, 1)
            .expect("retained")
            .map(|config| config.bytes.to_vec()),
        Some(vec![3])
    );
    assert_eq!(ring.track_config_at(2, MediaType::Audio, 0), Ok(None));
}

#[test]
fn recording_publication_starts_a_generation_at_the_packet_carrying_it() {
    let ring = RingBuffer::new(16);
    let mut batch = vec![
        video(true, &[SPS, PPS_A, IDR]),
        video(false, &[P_SLICE]),
        video(true, &[IDR]),
        video(false, &[P_SLICE]),
        video(true, &[SPS, PPS_A, IDR]),
        video(true, &[SPS, PPS_B, IDR]),
        packet(MediaType::Audio, 0, false, &[SPS, PPS_A].concat()),
    ];
    ring.push_drained_batch_recording_config(&mut batch);

    assert_eq!(generations(&ring), [1, 1, 1, 1, 1, 2, 0]);
    assert_eq!(
        ring.track_config_at(1, MediaType::Video, 0)
            .expect("retained")
            .map(|config| config.bytes.to_vec()),
        Some([SPS, PPS_A].concat()),
        "a packet before the change resolves to the configuration it was published with"
    );
    assert_eq!(ring.video_parameter_sets(), Some([SPS, PPS_B].concat()));

    let plain = RingBuffer::new(4);
    plain.push(video(true, &[SPS, PPS_A, IDR]));
    plain.push_batch([video(true, &[SPS, PPS_B, IDR])]);
    let mut capped = vec![video(true, &[SPS, PPS_A, IDR])];
    plain.push_drained_batch_capped(&mut capped);
    assert!(
        plain.video_config().is_none(),
        "plain publication never scans payloads"
    );

    // An FLV body whose NAL lengths read like start codes (length-1 units).
    let flv = RingBuffer::new(4);
    flv.push_batch_recording_config([MediaPacket {
        format: PayloadFormat::Flv,
        ..video(true, &[SPS, PPS_A, IDR])
    }]);
    assert!(
        flv.video_config().is_none(),
        "only Raw payloads carry Annex B parameter sets"
    );
}

/// A ring that republishes another ring's video starts each generation at
/// the packet where the source started it, publishing the packets queued
/// ahead of it first.
#[test]
fn a_follower_copies_the_source_configuration_at_its_packet() {
    let source = RingBuffer::new(16);
    source.push_drained_batch_recording_config(&mut vec![
        video(false, &[P_SLICE]),
        video(true, &[SPS, PPS_A, IDR]),
        video(false, &[P_SLICE]),
        video(true, &[SPS, PPS_A, IDR]),
        video(true, &[SPS, PPS_B, IDR]),
    ]);
    let target = RingBuffer::new(16);
    let mut follower = VideoConfigFollower::default();
    let mut pending = Vec::new();
    for sequence in 0..source.get_write_idx() {
        follower.follow(&source, sequence, &target, &mut pending);
        pending.push((*source.read_at(sequence).expect("published")).clone());
    }
    target.push_drained_batch_capped(&mut pending);

    assert_eq!(generations(&target), [0, 1, 1, 1, 2]);
    assert_eq!(
        target
            .track_config_at(2, MediaType::Video, 0)
            .expect("retained")
            .map(|config| config.bytes.to_vec()),
        Some([SPS, PPS_A].concat())
    );
    assert_eq!(target.video_parameter_sets(), Some([SPS, PPS_B].concat()));
}

#[test]
fn a_follower_behind_the_source_copies_its_current_configuration() {
    let source = RingBuffer::new(2);
    source.push_drained_batch_recording_config(&mut vec![
        video(true, &[SPS, PPS_A, IDR]),
        video(true, &[SPS, PPS_B, IDR]),
        video(false, &[P_SLICE]),
    ]);
    let target = RingBuffer::new(4);
    let mut pending = vec![video(false, &[P_SLICE])];
    VideoConfigFollower::default().follow(&source, 0, &target, &mut pending);

    assert!(pending.is_empty(), "packets ahead of the change go first");
    assert_eq!(generations(&target), [0]);
    assert_eq!(target.video_parameter_sets(), Some([SPS, PPS_B].concat()));
}

/// A present unit whose generation fell out of the source's history copies
/// the source's current configuration rather than keeping the old one.
#[test]
fn a_follower_copies_the_current_configuration_for_an_evicted_generation() {
    let source = RingBuffer::new(16);
    source.set_video_parameter_sets(vec![1]);
    source.push(video(true, &[IDR]));
    for bytes in 2..=CONFIG_HISTORY as u8 + 1 {
        source.set_video_parameter_sets(vec![bytes]);
    }
    assert_eq!(
        source.track_config_at(0, MediaType::Video, 0),
        Err(NotRetained)
    );
    let target = RingBuffer::new(4);

    VideoConfigFollower::default().follow(&source, 0, &target, &mut Vec::new());

    assert_eq!(
        target.video_parameter_sets(),
        Some(vec![CONFIG_HISTORY as u8 + 1])
    );
}

#[test]
fn frame_units_are_bounded_by_the_ring_and_survive_wrap_around() {
    let ring = RingBuffer::new(4);
    for generation in 1..=10u8 {
        ring.set_track_config(MediaType::Video, 0, vec![generation]);
        ring.push(packet(MediaType::Video, 0, true, &[0]));
    }

    assert_eq!(
        ring.frame_unit(6),
        None,
        "the slot the next publication overwrites is not retained (as in Reader::pull)"
    );
    assert_eq!(ring.frame_unit(10), None, "not yet published");
    let retained: Vec<u32> = (7..10)
        .map(|sequence| {
            ring.frame_unit(sequence)
                .expect("retained")
                .config_generation
        })
        .collect();
    assert_eq!(retained, [8, 9, 10]);
    assert_eq!(
        ring.track_config_at(5, MediaType::Video, 0),
        Err(NotRetained)
    );
}

/// A batch writes its slots before it stores the write index. A reader that
/// asks for a sequence the batch has already overwritten, while the write
/// index still says it is retained, gets nothing, never the overwriting
/// packet's unit.
#[test]
fn a_unit_overwritten_by_an_unfinished_batch_is_not_retained() {
    let ring = RingBuffer::new(4);
    for sequence in 0..12u8 {
        ring.set_track_config(MediaType::Video, 0, vec![sequence]);
        ring.push(packet(MediaType::Video, 0, true, &[0]));
    }
    let mut observed = Vec::new();
    let mut next = 12u8;
    ring.push_batch(std::iter::from_fn(|| {
        // Asked for the fourth packet: 12, 13 and 14 are in their slots
        // (14 over 10's) and the write index is still 12.
        if next == 15 {
            assert_eq!(ring.get_write_idx(), 12);
            observed.extend((9..12).map(|sequence| {
                (
                    ring.frame_unit(sequence),
                    ring.track_config_at(sequence, MediaType::Video, 0)
                        .map(|config| config.map(|config| config.bytes.to_vec())),
                )
            }));
            return None;
        }
        ring.set_track_config(MediaType::Video, 0, vec![next]);
        next += 1;
        Some(packet(MediaType::Video, 0, true, &[0]))
    }));

    assert_eq!(
        observed,
        [
            (None, Err(NotRetained)),
            (None, Err(NotRetained)),
            (
                Some(FrameUnit {
                    config_generation: 12
                }),
                Ok(Some(vec![11]))
            ),
        ],
        "9 and 10 are overwritten by 13 and 14; 11 is intact"
    );
}

#[test]
fn a_configuration_older_than_the_history_is_not_retained() {
    let ring = RingBuffer::new(64);
    for generation in 0..=CONFIG_HISTORY as u8 {
        ring.set_track_config(MediaType::Video, 0, vec![generation]);
        ring.push(packet(MediaType::Video, 0, true, &[0]));
    }

    assert_eq!(
        ring.track_config_at(0, MediaType::Video, 0),
        Err(NotRetained)
    );
    for sequence in 1..=CONFIG_HISTORY {
        let config = ring
            .track_config_at(sequence, MediaType::Video, 0)
            .expect("retained")
            .expect("recorded");
        assert_eq!(config.generation as usize, sequence + 1);
        assert_eq!(config.bytes.as_ref(), [sequence as u8]);
    }
}

#[test]
fn a_resized_ring_continues_units_and_generations() {
    let old = RingBuffer::new(4);
    let mut batch = vec![
        video(true, &[SPS, PPS_A, IDR]),
        video(false, &[P_SLICE]),
        video(true, &[SPS, PPS_B, IDR]),
        video(false, &[P_SLICE]),
        video(false, &[P_SLICE]),
    ];
    old.push_drained_batch_recording_config(&mut batch);

    let grown = RingBuffer::new_continuing(16, old.get_write_idx());
    grown.seed_readable_tail_from(&old);
    assert_eq!(grown.frame_unit(0), None, "an unseeded slot (raw 0, tag 0)");
    assert_eq!(grown.frame_unit(1), None, "a slot the resize did not seed");
    for sequence in 2..5 {
        assert_eq!(grown.frame_unit(sequence), old.frame_unit(sequence));
        assert_eq!(
            grown.track_config_at(sequence, MediaType::Video, 0),
            old.track_config_at(sequence, MediaType::Video, 0)
        );
    }

    let mut next = vec![video(true, &[SPS, PPS_B, IDR])];
    grown.push_drained_batch_recording_config(&mut next);
    assert_eq!(
        grown.frame_unit(5),
        Some(FrameUnit {
            config_generation: 2,
        }),
        "the same configuration keeps its generation across the resize"
    );
}

#[derive(Debug, Clone)]
enum Op {
    Set { track: (bool, u32), bytes: u8 },
    Push { track: (bool, u32) },
}

fn op() -> impl Strategy<Value = Op> {
    let track = (any::<bool>(), 0..CONFIG_TRACKS_PER_TYPE as u32 + 2);
    prop_oneof![
        (track.clone(), 0u8..3).prop_map(|(track, bytes)| Op::Set { track, bytes }),
        track.prop_map(|track| Op::Push { track }),
    ]
}

fn media_type(video: bool) -> MediaType {
    if video {
        MediaType::Video
    } else {
        MediaType::Audio
    }
}

#[derive(Default)]
struct TrackModel {
    generation: u32,
    configs: Vec<Vec<u8>>,
}

proptest! {
    /// The side table against a model that keeps every unit and every
    /// configuration: a lookup answers exactly when the ring still retains
    /// the packet and its generation, and then answers what the model says.
    #[test]
    fn frame_units_match_an_unbounded_model(
        capacity in 1usize..8,
        ops in prop::collection::vec(op(), 0..80),
    ) {
        let ring = RingBuffer::new(capacity);
        let mut tracks: std::collections::HashMap<(bool, u32), TrackModel> =
            std::collections::HashMap::new();
        let mut published: Vec<((bool, u32), FrameUnit)> = Vec::new();

        for op in ops {
            match op {
                Op::Set { track, bytes } => {
                    let bytes = vec![bytes; 3];
                    let generation =
                        ring.set_track_config(media_type(track.0), track.1, bytes.clone());
                    if (track.1 as usize) < CONFIG_TRACKS_PER_TYPE {
                        let model = tracks.entry(track).or_default();
                        if model.configs.last() != Some(&bytes) {
                            model.generation += 1;
                            model.configs.push(bytes);
                        }
                        prop_assert_eq!(generation, model.generation);
                    } else {
                        prop_assert_eq!(generation, 0);
                    }
                }
                Op::Push { track } => {
                    ring.push(packet(media_type(track.0), track.1, false, &[0]));
                    let config_generation = tracks.get(&track).map_or(0, |model| model.generation);
                    published.push((track, FrameUnit { config_generation }));
                }
            }
        }

        let head = published.len();
        for sequence in 0..head + 2 {
            let expected = published
                .get(sequence)
                .filter(|_| head - sequence < capacity);
            prop_assert_eq!(ring.frame_unit(sequence), expected.map(|(_, unit)| *unit));

            let Some(((video, index), unit)) = expected else {
                continue;
            };
            let lookup = ring.track_config_at(sequence, media_type(*video), *index);
            if unit.config_generation == 0 {
                prop_assert_eq!(lookup, Ok(None));
                continue;
            }
            let model = &tracks[&(*video, *index)];
            if model.generation - unit.config_generation < CONFIG_HISTORY as u32 {
                let config = lookup.expect("retained").expect("recorded");
                prop_assert_eq!(config.generation, unit.config_generation);
                prop_assert_eq!(
                    config.bytes.as_ref(),
                    model.configs[unit.config_generation as usize - 1].as_slice()
                );
            } else {
                prop_assert_eq!(lookup, Err(NotRetained));
            }
        }
    }
}
