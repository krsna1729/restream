use super::*;

// -- RingBuffer push/pull --

#[test]
fn push_then_pull_returns_packets_in_order() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let rb = Arc::new(RingBuffer::new(16));
        rb.push(video_packet(0, 0, true));
        rb.push(audio_packet(10, 10));
        rb.push(video_packet(33, 30, false));

        let mut reader = Reader::new("test".to_string(), rb);
        let p1 = reader.pull().unwrap().unwrap();
        assert_eq!(p1.pts, 0);
        assert!(p1.is_keyframe);

        let p2 = reader.pull().unwrap().unwrap();
        assert_eq!(p2.media_type, MediaType::Audio);
        assert_eq!(p2.pts, 10);

        let p3 = reader.pull().unwrap().unwrap();
        assert_eq!(p3.pts, 33);

        assert!(reader.pull().unwrap().is_none());
    });
}

#[test]
fn push_batch_then_pull_burst_returns_packets_in_order() {
    let ring = Arc::new(RingBuffer::new(16));
    let published = ring.push_batch([
        video_packet(10, 10, true),
        video_packet(20, 20, false),
        video_packet(30, 30, false),
    ]);
    assert_eq!(published, 3);
    assert_eq!(ring.get_write_idx(), 3);

    let mut reader = Reader::new("test_burst".to_string(), ring);
    let mut packets = Vec::new();
    assert_eq!(reader.pull_burst(&mut packets, 2).unwrap(), 2);
    assert_eq!(reader.pull_burst(&mut packets, 2).unwrap(), 1);
    assert_eq!(
        packets.iter().map(|packet| packet.pts).collect::<Vec<_>>(),
        vec![10, 20, 30]
    );
}

#[test]
fn empty_batch_does_not_advance_ring() {
    let ring = RingBuffer::new(16);
    assert_eq!(ring.push_batch(std::iter::empty()), 0);
    assert_eq!(ring.get_write_idx(), 0);
}

#[test]
fn reader_starts_at_last_keyframe() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let rb = Arc::new(RingBuffer::new(64));
        // Push some packets, including keyframes at different positions
        for i in 0..20 {
            rb.push(video_packet(i * 33, i * 33, i % 10 == 0)); // KF at 0, 10
        }
        rb.push(audio_packet(660, 660));

        let mut reader = Reader::new("test_starts".to_string(), rb);
        // Should start at or after the last keyframe (index 10)
        let first = reader.pull().unwrap().unwrap();
        assert!(first.pts >= 10 * 33);
    });
}

#[test]
fn multiple_readers_pull_same_packets() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let rb = Arc::new(RingBuffer::new(64));
        rb.push(video_packet(0, 0, true));
        rb.push(video_packet(33, 33, false));

        let mut r1 = Reader::new("r1".to_string(), rb.clone());
        let mut r2 = Reader::new("r2".to_string(), rb.clone());

        let p1 = r1.pull().unwrap().unwrap();
        let p2 = r2.pull().unwrap().unwrap();
        assert_eq!(p1.pts, p2.pts);
        assert_eq!(p1.dts, p2.dts);
    });
}

#[test]
fn fill_and_capacity_reports_correct_values() {
    let rb = RingBuffer::new(16);
    assert_eq!(rb.fill_and_capacity(), (0, 16));

    rb.push(video_packet(0, 0, true));
    assert_eq!(rb.fill_and_capacity(), (1, 16));

    for i in 1..16 {
        rb.push(audio_packet(i, i));
    }
    assert_eq!(rb.fill_and_capacity(), (16, 16));

    // After wrapping, fill stays at capacity
    rb.push(audio_packet(100, 100));
    assert_eq!(rb.fill_and_capacity(), (16, 16));
}

#[test]
fn reader_snapshots_report_lag_overflow_and_packet_age() {
    let rb = Arc::new(RingBuffer::new(4));
    rb.push(video_packet(0, 0, true));
    let mut reader = Reader::new("slow-reader".to_string(), rb.clone());

    std::thread::sleep(std::time::Duration::from_millis(2));
    rb.push(audio_packet(10, 10));
    rb.push(audio_packet(20, 20));

    let snapshots = rb.reader_snapshots();
    assert_eq!(snapshots.len(), 1);
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.name, "slow-reader");
    assert_eq!(snapshot.lag_slots, 3);
    assert_eq!(snapshot.overflow_count, 0);
    assert!(
        snapshot.packet_age_ms.is_some(),
        "lagging reader should report the age of its next unread packet"
    );

    for i in 3..8 {
        rb.push(audio_packet(i * 10, i * 10));
    }
    assert!(reader.pull().is_err());

    let snapshots = rb.reader_snapshots();
    assert_eq!(snapshots[0].overflow_count, 1);
}

#[test]
fn video_parameter_sets_cache_supports_late_complete_update() {
    let rb = RingBuffer::new(16);
    rb.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ]);
    assert_eq!(
        rb.video_parameter_sets(),
        Some(vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ])
    );

    rb.set_video_parameter_sets(vec![
        0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB, 0x00,
        0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
    ]);
    assert_eq!(
        rb.video_parameter_sets(),
        Some(vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ]),
        "later complete parameter sets should replace an earlier partial cache"
    );
}

// -- DtsEnforcer --

#[test]
fn dts_enforcer_passes_through_increasing_dts() {
    let mut e = DtsEnforcer::new(2);
    assert_eq!(e.enforce(0, 0, 0), (0, 0));
    assert_eq!(e.enforce(0, 33, 33), (33, 33));
    assert_eq!(e.enforce(0, 66, 66), (66, 66));
}

#[test]
fn dts_enforcer_bumps_equal_dts() {
    let mut e = DtsEnforcer::new(2);
    // Two audio packets with the same DTS (common at ms granularity)
    assert_eq!(e.enforce(1, 10, 10), (10, 10));
    assert_eq!(e.enforce(1, 10, 10), (11, 11)); // bumped
    assert_eq!(e.enforce(1, 10, 10), (12, 12)); // bumped again
}

#[test]
fn dts_enforcer_bumps_decreasing_dts() {
    let mut e = DtsEnforcer::new(1);
    assert_eq!(e.enforce(0, 100, 100), (100, 100));
    assert_eq!(e.enforce(0, 50, 50), (101, 101)); // backwards jump corrected
}

#[test]
fn dts_enforcer_adjusts_pts_below_dts() {
    let mut e = DtsEnforcer::new(1);
    assert_eq!(e.enforce(0, 100, 100), (100, 100));
    // PTS=90, DTS=90 → DTS bumped to 101, PTS raised to 101
    assert_eq!(e.enforce(0, 90, 90), (101, 101));
}

#[test]
fn dts_enforcer_preserves_pts_cts_offset() {
    let mut e = DtsEnforcer::new(1);
    // B-frame pattern: PTS ahead of DTS (composition time offset)
    assert_eq!(e.enforce(0, 132, 99), (132, 99));
    assert_eq!(e.enforce(0, 165, 132), (165, 132));
}

#[test]
fn dts_enforcer_independent_per_stream() {
    let mut e = DtsEnforcer::new(2);
    assert_eq!(e.enforce(0, 100, 100), (100, 100));
    // Stream 1 has its own DTS tracking
    assert_eq!(e.enforce(1, 50, 50), (50, 50));
    // Stream 0 continues from 100
    assert_eq!(e.enforce(0, 100, 100), (101, 101));
}

#[test]
fn dts_enforcer_handles_out_of_bounds_stream() {
    let mut e = DtsEnforcer::new(1);
    // Stream index 5 is out of bounds — passes through unchanged
    assert_eq!(e.enforce(5, 100, 100), (100, 100));
}

#[test]
fn dts_enforcer_stream_idx_collision_corrupts_video_dts() {
    // Regression for issue #2: before the fix, audio packets with an
    // unknown track_index were routed to stream_idx=0 via `.unwrap_or(0)`,
    // aliasing into the video DTS slot. This test documents the corruption
    // pattern. The fix is `None => continue` in all pipeline mux loops so
    // unknown-track audio packets are dropped instead of aliased.
    let mut e = DtsEnforcer::new(2); // stream 0 = video, stream 1 = audio

    // Normal video frame.
    assert_eq!(e.enforce(0, 100, 100), (100, 100));

    // Simulate the OLD bug: an audio packet with unknown track_index is
    // incorrectly routed to stream_idx=0 (video's slot) and carries a
    // large DTS, advancing video's monotonic counter to 300.
    assert_eq!(e.enforce(0, 300, 300), (300, 300));

    // Next genuine video frame at dts=200 is now bumped to 301 instead of
    // passing through at 200, breaking A/V sync. With `None => continue`
    // the audio packet is skipped so the video counter stays at 100 and
    // dts=200 passes through correctly.
    let (_, corrupted) = e.enforce(0, 200, 200);
    assert_eq!(
        corrupted, 301,
        "aliasing audio to stream_idx=0 bumps video DTS past the actual \
             video timestamp, demonstrating the corruption fixed by None=>continue"
    );
}

#[test]
fn test_min_read_idx_reporting() {
    let rb = Arc::new(RingBuffer::new(16));
    assert_eq!(rb.min_read_idx(), 0);

    rb.push(video_packet(0, 0, true));
    let r1 = Reader::new("r1".into(), rb.clone());
    let r2 = Reader::new("r2".into(), rb.clone());

    // Both readers start at last keyframe (0)
    assert_eq!(rb.min_read_idx(), 0);

    // Advance r1 by pulling packet
    let mut r1 = r1;
    let mut r2 = r2;
    let _ = r1.pull().unwrap();
    assert_eq!(rb.min_read_idx(), 0); // min remains 0 since r2 is at 0

    let _ = r2.pull().unwrap();
    assert_eq!(rb.min_read_idx(), 1); // both are now at 1
}

// ── Vec pre-allocation correctness ───────────────────────────────

#[test]
fn vec_with_capacity_retains_capacity_after_clear() {
    let cap = 65536;
    let mut v: Vec<u8> = Vec::with_capacity(cap);
    assert!(v.capacity() >= cap);
    v.extend_from_slice(&[0x47u8; 1000]);
    assert!(!v.is_empty());
    assert!(v.capacity() >= cap);
    v.clear();
    assert!(v.is_empty());
    assert!(v.capacity() >= cap);
}

#[test]
fn vec_with_capacity_retains_capacity_after_drain() {
    let cap = 32;
    let mut v: Vec<(usize, bool)> = Vec::with_capacity(cap);
    for i in 0..10 {
        v.push((i, i == 0));
    }
    let cap_before = v.capacity();
    let drained_len = v.drain(..).count();
    assert_eq!(drained_len, 10);
    assert!(v.is_empty());
    assert_eq!(v.capacity(), cap_before);
}

#[test]
fn vec_new_has_zero_capacity() {
    let v: Vec<u8> = Vec::new();
    assert_eq!(v.capacity(), 0);
}

#[test]
fn vec_with_capacity_reuses_allocation_across_cycles() {
    let cap = 65536;
    let mut v: Vec<u8> = Vec::with_capacity(cap);
    let alloc_id = v.as_ptr() as usize;
    for _ in 0..3 {
        v.extend_from_slice(&[0x47u8; 1000]);
        v.clear();
        assert_eq!(v.as_ptr() as usize, alloc_id);
    }
}

#[test]
fn pull_burst_records_burst_stats() {
    let rb = Arc::new(RingBuffer::new(64));
    // Push 5 packets so first burst yields 5, then push 1 for a size-1 burst.
    for i in 0i64..5 {
        rb.push(video_packet(i * 33, i * 33, i == 0));
    }
    let mut reader = Reader::new("stats_test".to_string(), rb.clone());
    let mut out = Vec::new();

    // Burst of 5
    let n = reader.pull_burst(&mut out, 32).unwrap();
    assert_eq!(n, 5);

    // Push 1 more; burst of 1
    rb.push(video_packet(5_i64 * 33, 5_i64 * 33, false));
    let n2 = reader.pull_burst(&mut out, 32).unwrap();
    assert_eq!(n2, 1);

    let (avg, median, bursts) = reader.info.burst_stats();
    assert_eq!(bursts, 2, "two non-empty burst calls");
    // avg = (5+1)/2 = 3.0
    assert!((avg - 3.0).abs() < 0.01, "avg burst = {avg}");
    // n=5 lands in the 5-8 bucket and n=1 lands in the size-1 bucket.
    // The approximate median walks buckets by count, so the size-1 bucket wins.
    assert_eq!(median, 1, "median burst = {median}");

    // Empty pull does not record a burst
    let n3 = reader.pull_burst(&mut out, 32).unwrap();
    assert_eq!(n3, 0);
    let (_, _, bursts2) = reader.info.burst_stats();
    assert_eq!(bursts2, 2, "empty pull must not increment burst_count");
}

#[test]
fn payload_stats_reports_retained_ring_bytes() {
    let rb = Arc::new(RingBuffer::new(3));
    rb.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: Bytes::from(vec![1; 10]),
    });
    rb.push(MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: Bytes::from(vec![2; 4]),
    });

    let stats = rb.payload_stats();
    assert_eq!(stats.slots, 2);
    assert_eq!(stats.payload_bytes, 14);
    assert_eq!(stats.video_bytes, 10);
    assert_eq!(stats.audio_bytes, 4);
    assert_eq!(stats.min_payload_bytes, 4);
    assert_eq!(stats.max_payload_bytes, 10);
}

#[test]
fn observed_payload_bitrate_uses_retained_media_time() {
    let rb = RingBuffer::new(8);
    for dts in [0, 250, 500, 750, 1000] {
        rb.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: dts,
            dts,
            is_keyframe: dts == 0,
            format: PayloadFormat::Raw,
            payload: Bytes::from(vec![0; 37_500]),
        });
    }

    assert_eq!(rb.observed_payload_bitrate_bps(), Some(1_500_000));
}

#[test]
fn observed_payload_bitrate_uses_peak_media_window_for_bursty_startup() {
    let rb = RingBuffer::new(8);
    for (dts, bytes) in [
        (0, 80_000),
        (33, 5_000),
        (66, 5_000),
        (1000, 5_000),
        (1033, 5_000),
        (1066, 5_000),
    ] {
        rb.push(media_packet_with_payload(MediaType::Video, dts, bytes));
    }

    assert_eq!(rb.observed_payload_bitrate_bps(), Some(2_880_000));
}

#[test]
fn observed_payload_bitrate_sorts_by_dts_not_arrival_order() {
    let in_order = RingBuffer::new(8);
    for dts in [0, 250, 500, 750, 1000] {
        in_order.push(media_packet_with_payload(MediaType::Video, dts, 10_000));
    }

    let reordered = RingBuffer::new(8);
    for dts in [500, 0, 1000, 250, 750] {
        reordered.push(media_packet_with_payload(MediaType::Video, dts, 10_000));
    }

    assert_eq!(
        reordered.observed_payload_bitrate_bps(),
        in_order.observed_payload_bitrate_bps()
    );
}

#[test]
fn observed_payload_bitrate_includes_same_dts_audio_payloads() {
    let rb = RingBuffer::new(8);
    for dts in [0, 500, 1000] {
        rb.push(media_packet_with_payload(MediaType::Video, dts, 20_000));
        rb.push(media_packet_with_payload(MediaType::Audio, dts, 5_000));
    }

    assert_eq!(rb.observed_payload_bitrate_bps(), Some(800_000));
}

#[test]
fn observed_payload_bitrate_ignores_long_gaps_when_peak_is_higher() {
    let rb = RingBuffer::new(8);
    for (dts, bytes) in [(0, 40_000), (33, 40_000), (10_000, 1_000), (10_033, 1_000)] {
        rb.push(media_packet_with_payload(MediaType::Video, dts, bytes));
    }

    assert_eq!(rb.observed_payload_bitrate_bps(), Some(2_560_000));
}

#[test]
fn observed_payload_bitrate_rejects_a_too_short_window() {
    let rb = RingBuffer::new(4);
    rb.push(video_packet(0, 0, true));
    rb.push(video_packet(100, 100, false));

    assert_eq!(rb.observed_payload_bitrate_bps(), None);
}

#[test]
fn buffer_depth_secs_requires_estimated_pkt_rate() {
    let rb = RingBuffer::new(1024);
    assert_eq!(rb.buffer_depth_secs(), None, "no rate set yet");

    rb.set_estimated_pkt_rate(80.0); // 1080p30 + 1 audio
    let depth = rb.buffer_depth_secs().expect("rate is set");
    // 1024 slots / 80 pkt/s = 12.8 s
    assert!((depth - 12.8).abs() < 0.1, "depth={depth}");
}

#[test]
fn buffer_depth_secs_multi_track_stream() {
    let rb = RingBuffer::new(4980); // adaptive size for 830 pkt/s × 6 s
    rb.set_estimated_pkt_rate(830.0); // 30fps video + 16 audio tracks × 50
    let depth = rb.buffer_depth_secs().unwrap();
    // 4980 / 830 ≈ 6.0 s
    assert!((depth - 6.0).abs() < 0.1, "depth={depth}");
}
