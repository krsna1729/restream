use super::*;

#[test]
fn overflow_triggers_fast_forward_to_keyframe() {
    let rb = Arc::new(RingBuffer::new(8));

    let info = Arc::new(ReaderInfo::new("test_overflow".to_string(), 0));
    let mut reader = Reader {
        buffer: rb.clone(),
        info,
        read_idx: 0,
        migration_preroll_packets: 0,
    };

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
    // Bug #8: when no keyframe has been pushed yet (sentinel = usize::MAX)
    // fast_forward must return current_write_idx, NOT 0 or write_idx.saturating_sub(100).
    // Returning 0 when write_idx < 100 caused late-joining readers to re-scan
    // from the beginning of the ring rather than starting at the live edge.
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
