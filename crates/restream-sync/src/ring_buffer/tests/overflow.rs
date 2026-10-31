use super::*;

#[test]
fn overflow_triggers_fast_forward_to_keyframe() {
    let rb = Arc::new(RingBuffer::new(8));

    let mut reader = Reader::new_live("test_overflow".to_string(), rb.clone());

    // Push 20 packets with a keyframe at index 15
    for i in 0..20 {
        rb.push(video_packet(i * 33, i * 33, i == 0 || i == 15));
    }
    // write_idx=20, reader at 0, gap=20 >= capacity=8 → overflow

    let result = reader.pull();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("Overflow"));
    assert_eq!(reader.info.overflow_count.load(Ordering::Relaxed), 1);
}

#[test]
fn fast_forward_with_no_keyframe_returns_live_edge() {
    // When no keyframe has been pushed yet (sentinel = usize::MAX)
    // fast_forward must return current_write_idx, NOT 0 or write_idx.saturating_sub(100).
    // Returning 0 when write_idx < 100 would make late-joining readers re-scan
    // from the beginning of the ring rather than start at the live edge.
    let rb = Arc::new(RingBuffer::new(4096));

    // Push 5 non-keyframe audio packets (no video keyframe → sentinel stays)
    for i in 0..5 {
        rb.push(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: i * 10,
            dts: i * 10,
            is_keyframe: false,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from_static(b"\xAA"),
        });
    }
    // write_idx is now 5; no keyframe has been seen → sentinel usize::MAX
    let write_idx = rb.write_idx.val.load(Ordering::Relaxed);
    assert_eq!(write_idx, 5);

    // fast_forward should return write_idx (live edge), not 0
    let ff = rb.fast_forward(write_idx);
    assert_eq!(
        ff, write_idx,
        "fast_forward with no keyframe must return the live edge, not 0"
    );
}

#[test]
fn fast_forward_with_keyframe_at_slot_zero() {
    // When the very first packet pushed is a video keyframe (idx=0),
    // fast_forward must still be able to find that keyframe (not confuse it
    // with the "no keyframe" sentinel).
    let rb = Arc::new(RingBuffer::new(4096));

    // Push one keyframe at slot 0
    rb.push(video_packet(0, 0, true));
    let write_idx = rb.write_idx.val.load(Ordering::Relaxed);
    assert_eq!(write_idx, 1);

    // fast_forward should return 0 (the keyframe slot), not 1 (live edge)
    let ff = rb.fast_forward(write_idx);
    assert_eq!(ff, 0, "fast_forward should return the keyframe at slot 0");
}

#[test]
fn keyframe_preroll_reader_starts_before_last_keyframe_when_available() {
    let rb = Arc::new(RingBuffer::new(64));
    for i in 0..8 {
        rb.push(video_packet(i * 10, i * 10, i == 5));
    }

    let reader = Reader::new_with_keyframe_preroll("preroll".into(), rb, 2);
    assert_eq!(
        reader.read_idx, 3,
        "reader should keep a small preroll window ahead of the last keyframe"
    );
}

#[test]
fn fault_injection_empty_payload_does_not_panic() {
    let rb = Arc::new(RingBuffer::new(16));
    rb.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: Bytes::new(),
    });
    let mut reader = Reader::new("empty".to_string(), rb);
    let pkt = reader.pull().unwrap().unwrap();
    assert!(pkt.payload.is_empty());
}

#[test]
fn fault_injection_reordered_dts_does_not_corrupt_ring() {
    let rb = Arc::new(RingBuffer::new(16));
    rb.push(video_packet(100, 100, true));
    rb.push(video_packet(50, 50, false));
    rb.push(video_packet(200, 200, false));

    let mut reader = Reader::new("reorder".to_string(), rb);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.dts, 100);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.dts, 50);
    let p3 = reader.pull().unwrap().unwrap();
    assert_eq!(p3.dts, 200);
}

#[test]
fn fault_injection_large_timestamp_gap_handled() {
    let rb = Arc::new(RingBuffer::new(16));
    rb.push(video_packet(0, 0, true));
    rb.push(video_packet(i64::MAX - 1, i64::MAX - 1, false));
    rb.push(video_packet(i64::MIN, i64::MIN, false));

    let mut reader = Reader::new("gap".to_string(), rb);
    assert!(reader.pull().unwrap().is_some());
    assert!(reader.pull().unwrap().is_some());
    assert!(reader.pull().unwrap().is_some());
    assert!(reader.pull().unwrap().is_none());
}

#[test]
fn fault_injection_negative_timestamps_handled() {
    let rb = Arc::new(RingBuffer::new(16));
    rb.push(video_packet(-100, -100, true));
    rb.push(audio_packet(-50, -50));
    rb.push(video_packet(0, 0, false));

    let mut reader = Reader::new("negative".to_string(), rb);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.pts, -100);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.pts, -50);
    let p3 = reader.pull().unwrap().unwrap();
    assert_eq!(p3.pts, 0);
}

#[test]
fn fault_injection_rapid_overflow_recovery() {
    let rb = Arc::new(RingBuffer::new(4));
    rb.push(video_packet(0, 0, true));
    let mut reader = Reader::new("overflow_recovery".to_string(), rb.clone());

    for i in 1..20 {
        rb.push(video_packet(i * 33, i * 33, i == 10));
    }

    let err = reader.pull().unwrap_err();
    assert!(err.contains("Overflow"));

    rb.push(video_packet(1000, 1000, true));
    rb.push(video_packet(1033, 1033, false));

    let pkt = reader.pull().unwrap().unwrap();
    assert!(pkt.pts >= 1000);
}

/// A batch writes its slots before it stores the write index, so while it
/// is unfinished a reader whose lag is still "retained" by the write index
/// can find the next lap's packet in its slot. The reader reports an
/// overflow; it never returns a packet in place of the one it asked for.
#[test]
fn a_reader_never_returns_a_packet_an_unfinished_batch_overwrote() {
    let ring = Arc::new(RingBuffer::new(4));
    for sequence in 0..12 {
        ring.push(video_packet(sequence, sequence, sequence == 0));
    }
    let mut reader = Reader::new_live("lapped".to_string(), ring.clone());
    reader.sync_read_idx(9);
    let mut pulled = None;
    let mut next = 12;
    ring.push_batch(std::iter::from_fn(|| {
        // 12, 13 and 14 are in their slots (13 over 9's, 14 over 10's) and
        // the write index is still 12.
        if next == 15 {
            assert_eq!(ring.get_write_idx(), 12);
            let mut out = Vec::new();
            let result = reader.pull_burst(&mut out, 32);
            pulled = Some((result, out));
            return None;
        }
        next += 1;
        Some(video_packet(next - 1, next - 1, false))
    }));

    let (result, out) = pulled.expect("pulled mid-batch");
    let returned: Vec<i64> = out.iter().map(|packet| packet.pts).collect();
    assert!(
        result.is_err(),
        "lapped slots are an overflow: {returned:?}"
    );
    assert!(
        returned.is_empty(),
        "no packet of another lap: {returned:?}"
    );
    assert_eq!(reader.info.overflow_count.load(Ordering::Relaxed), 1);
}

/// After an overflow with no keyframe left in the window the reader resumes
/// at the live edge. The interframes that follow reference frames it never
/// delivered: it skips video until the next keyframe, and audio passes.
#[test]
fn an_overflow_without_a_retained_keyframe_skips_interframes_until_the_next_keyframe() {
    let ring = Arc::new(RingBuffer::new(8));
    let mut reader = Reader::new_live("resync".to_string(), ring.clone());
    for sequence in 0..10 {
        ring.push(video_packet(sequence, sequence, sequence == 0));
    }
    let mut out = Vec::new();
    assert!(reader.pull_burst(&mut out, 32).is_err(), "overflow");

    ring.push(video_packet(10, 10, false));
    ring.push(audio_packet(11, 11));
    ring.push(video_packet(12, 12, true));
    ring.push(video_packet(13, 13, false));
    let mut delivered = Vec::new();
    while reader.lag() > 0 {
        out.clear();
        reader
            .pull_burst(&mut out, 32)
            .expect("no further overflow");
        delivered.extend(out.iter().map(|packet| (packet.media_type, packet.pts)));
    }

    assert_eq!(
        delivered,
        [
            (MediaType::Audio, 11),
            (MediaType::Video, 12),
            (MediaType::Video, 13)
        ]
    );
}

/// `pull` follows the same rule as `pull_burst` after an overflow.
#[test]
fn pull_after_an_overflow_without_a_retained_keyframe_starts_at_the_next_keyframe() {
    let ring = Arc::new(RingBuffer::new(4));
    let mut reader = Reader::new_live("resync-pull".to_string(), ring.clone());
    for sequence in 0..10 {
        ring.push(video_packet(sequence, sequence, sequence == 0));
    }
    assert!(reader.pull().is_err(), "overflow");

    ring.push(video_packet(10, 10, false));
    ring.push(video_packet(11, 11, true));

    assert_eq!(reader.pull().unwrap().map(|packet| packet.pts), Some(11));
}

/// The packets a reader must deliver from `start` after an overflow: every
/// one except the video interframes before the first video keyframe.
fn keyframe_wait_model(units: &[(bool, bool)], start: usize) -> Vec<usize> {
    let mut awaiting = true;
    (start..units.len())
        .filter(|&sequence| {
            let (video, keyframe) = units[sequence];
            if awaiting && video {
                if !keyframe {
                    return false;
                }
                awaiting = false;
            }
            true
        })
        .collect()
}

proptest::proptest! {
    /// Any media after an overflow, published in any chunks and read with
    /// `pull` or `pull_burst` at any budget: the reader delivers exactly the
    /// model's packets, each burst a contiguous run ending at
    /// `next_sequence`.
    #[test]
    fn reads_after_an_overflow_follow_the_keyframe_wait_model(
        units in proptest::collection::vec((proptest::bool::ANY, proptest::bool::ANY), 20..120),
        chunk in 1usize..8,
        budget in 1usize..8,
        use_pull in proptest::bool::ANY,
    ) {
        // (video, keyframe) per sequence; pts is the sequence.
        let packet = |sequence: usize| {
            let (video, keyframe) = units[sequence];
            let pts = sequence as i64;
            if video {
                video_packet(pts, pts, keyframe)
            } else {
                audio_packet(pts, pts)
            }
        };
        let ring = Arc::new(RingBuffer::new(16));
        let mut reader = Reader::new_live("model".to_string(), ring.clone());
        // The first 20 packets lap the reader in a 16-slot ring.
        for sequence in 0..20 {
            ring.push(packet(sequence));
        }
        let mut out = Vec::new();
        proptest::prop_assert!(reader.pull_burst(&mut out, budget).is_err());
        let start = reader.next_sequence();

        let mut delivered = Vec::new();
        let mut published = 20;
        loop {
            while reader.lag() > 0 {
                if use_pull {
                    if let Some(packet) = reader.pull().expect("no further overflow") {
                        proptest::prop_assert_eq!(packet.pts as usize, reader.next_sequence() - 1);
                        delivered.push(packet.pts as usize);
                    }
                } else {
                    out.clear();
                    reader.pull_burst(&mut out, budget).expect("no further overflow");
                    proptest::prop_assert!(out.len() <= budget);
                    let end = reader.next_sequence();
                    let run: Vec<usize> = out.iter().map(|packet| packet.pts as usize).collect();
                    proptest::prop_assert_eq!(&run, &(end - run.len()..end).collect::<Vec<_>>());
                    delivered.extend(run);
                }
            }
            if published == units.len() {
                break;
            }
            let next = (published + chunk).min(units.len());
            for sequence in published..next {
                ring.push(packet(sequence));
            }
            published = next;
        }
        proptest::prop_assert_eq!(delivered, keyframe_wait_model(&units, start));
    }
}

#[test]
fn fault_injection_mixed_format_payloads() {
    let rb = Arc::new(RingBuffer::new(16));
    rb.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Flv,
        payload: Bytes::from_static(&[0x17, 0x01, 0, 0, 0]),
    });
    rb.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 33,
        dts: 33,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
    });

    let mut reader = Reader::new("mixed_fmt".to_string(), rb);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.format, PayloadFormat::Flv);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.format, PayloadFormat::Raw);
}

#[test]
fn fault_injection_high_track_index() {
    let rb = Arc::new(RingBuffer::new(16));
    rb.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: u32::MAX,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0; 4]),
    });
    let mut reader = Reader::new("high_track".to_string(), rb);
    let pkt = reader.pull().unwrap().unwrap();
    assert_eq!(pkt.track_index, u32::MAX);
}

#[test]
fn fault_injection_push_batch_with_no_keyframes() {
    let rb = Arc::new(RingBuffer::new(16));
    let mut reader = Reader::new_live("no_kf".to_string(), rb.clone());
    let count = rb.push_batch([
        video_packet(10, 10, false),
        video_packet(20, 20, false),
        video_packet(30, 30, false),
    ]);
    assert_eq!(count, 3);
    let mut out = Vec::new();
    let n = reader.pull_burst(&mut out, 32).unwrap();
    assert_eq!(n, 3);
}

#[test]
fn dts_enforcer_fault_injection_extreme_backwards_jump() {
    let mut enforcer = DtsEnforcer::new(1);
    assert_eq!(
        enforcer.enforce(0, 1_000_000, 1_000_000),
        (1_000_000, 1_000_000)
    );
    let (pts, dts) = enforcer.enforce(0, 0, 0);
    assert!(dts > 1_000_000, "DTS must be bumped past previous value");
    assert!(pts >= dts, "PTS must be >= DTS");
}

#[test]
fn dts_enforcer_bump_at_i64_max_does_not_overflow() {
    let mut enforcer = DtsEnforcer::new(1);
    assert_eq!(
        enforcer.enforce(0, i64::MAX, i64::MAX),
        (i64::MAX, i64::MAX)
    );
    let (pts, dts) = enforcer.enforce(0, i64::MAX, i64::MAX);
    assert_eq!(
        dts,
        i64::MAX,
        "DTS must saturate at i64::MAX rather than wrap below the previous value on overflow"
    );
    assert!(pts >= dts, "PTS must be >= DTS");
}

#[test]
fn dts_enforcer_fault_injection_negative_pts_dts() {
    let mut enforcer = DtsEnforcer::new(1);
    let (pts, dts) = enforcer.enforce(0, -100, -100);
    assert_eq!((pts, dts), (-100, -100));
    let (next_pts, next_dts) = enforcer.enforce(0, -200, -200);
    assert!(next_dts > -100, "DTS must be bumped past -100");
    assert!(next_pts >= next_dts);
}
