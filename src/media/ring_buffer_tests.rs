use super::*;
use bytes::Bytes;
use std::sync::Mutex;

static EXPECTED_PANIC_HOOK_LOCK: Mutex<()> = Mutex::new(());

type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

struct ScopedSilentPanicHook(Option<PanicHook>);

impl ScopedSilentPanicHook {
    fn new() -> Self {
        Self(Some(std::panic::take_hook()))
    }

    fn silence(&mut self) {
        std::panic::set_hook(Box::new(|_| {}));
    }
}

impl Drop for ScopedSilentPanicHook {
    fn drop(&mut self) {
        if let Some(hook) = self.0.take() {
            std::panic::set_hook(hook);
        }
    }
}

fn video_packet(pts: i64, dts: i64, keyframe: bool) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts,
        dts,
        is_keyframe: keyframe,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0; 16]),
    }
}

fn audio_packet(pts: i64, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Audio,
        track_index: 0,
        pts,
        dts,
        is_keyframe: false,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(&[0; 4]),
    }
}

fn media_packet_with_payload(media_type: MediaType, dts: i64, payload_bytes: usize) -> MediaPacket {
    MediaPacket {
        media_type,
        track_index: 0,
        pts: dts,
        dts,
        is_keyframe: matches!(media_type, MediaType::Video) && dts == 0,
        format: PayloadFormat::Raw,
        payload: Bytes::from(vec![0; payload_bytes]),
    }
}

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

// -- Reader::drop lifecycle --

#[test]
fn reader_drop_removes_entry_from_readers_list() {
    let rb = Arc::new(RingBuffer::new(16));

    assert_eq!(
        rb.readers.lock().unwrap_or_else(|e| e.into_inner()).len(),
        0
    );

    let r1 = Reader::new("r1".into(), rb.clone());
    let r2 = Reader::new("r2".into(), rb.clone());
    assert_eq!(
        rb.readers.lock().unwrap_or_else(|e| e.into_inner()).len(),
        2
    );

    drop(r1);
    // After drop, our entry is removed and no stale Weak remains.
    assert_eq!(
        rb.readers.lock().unwrap_or_else(|e| e.into_inner()).len(),
        1
    );

    drop(r2);
    assert_eq!(
        rb.readers.lock().unwrap_or_else(|e| e.into_inner()).len(),
        0
    );
}

#[test]
fn reader_drop_cleans_up_on_poisoned_mutex() {
    // If another thread panics while holding readers.lock(), the mutex
    // becomes poisoned. The previous `if let Ok()` would skip cleanup,
    // leaving a stale Weak in the list. unwrap_or_else recovers the poison
    // and performs the cleanup correctly.
    let rb = Arc::new(RingBuffer::new(16));
    let r = Reader::new("r".into(), rb.clone());

    // Deliberately poison the mutex from another thread.
    let rb2 = rb.clone();
    let _panic_hook_lock = EXPECTED_PANIC_HOOK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut panic_hook = ScopedSilentPanicHook::new();
    panic_hook.silence();
    let poison_thread = std::thread::spawn(move || {
        let _guard = rb2.readers.lock().unwrap();
        panic!("intentional poison");
    });
    let _ = poison_thread.join(); // returns Err (panicked), mutex is now poisoned

    // Verify mutex is poisoned.
    assert!(rb.readers.lock().is_err());

    // Drop should NOT silently skip: it must clean up via unwrap_or_else.
    drop(r);

    // After drop, the list is empty even though the mutex was poisoned.
    assert_eq!(
        rb.readers.lock().unwrap_or_else(|e| e.into_inner()).len(),
        0
    );
}

#[test]
fn reader_drop_also_prunes_other_stale_weaks() {
    // Simulate a stale Weak that was left behind (e.g. from a previous bug)
    // by manually inserting one, then verifying drop cleans it.
    let rb = Arc::new(RingBuffer::new(16));
    {
        // Insert a Weak that immediately becomes stale.
        let ephemeral = Arc::new(ReaderInfo::new("stale".into(), 0));
        rb.readers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::downgrade(&ephemeral));
        // ephemeral drops here → Weak becomes stale
    }
    assert_eq!(
        rb.readers.lock().unwrap_or_else(|e| e.into_inner()).len(),
        1
    ); // stale entry present

    let r = Reader::new("live".into(), rb.clone());
    assert_eq!(
        rb.readers.lock().unwrap_or_else(|e| e.into_inner()).len(),
        2
    ); // stale + live

    drop(r);
    // drop() removes our entry AND prunes the stale one.
    assert_eq!(
        rb.readers.lock().unwrap_or_else(|e| e.into_inner()).len(),
        0
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

#[test]
fn test_concurrent_writer_reader_no_corruption() {
    let rb = Arc::new(RingBuffer::new(4));
    rb.push(video_packet(0, 0, true));
    let mut reader = Reader::new("r1".into(), rb.clone());

    let rb_c = rb.clone();
    let writer_handle = std::thread::spawn(move || {
        for i in 1..1000 {
            rb_c.push(video_packet(i * 10, i * 10, i % 10 == 0));
            std::thread::yield_now();
        }
    });

    for _ in 0..2000 {
        match reader.pull() {
            Ok(Some(p)) => {
                assert!(p.pts >= 0);
            }
            Ok(None) => {
                std::thread::yield_now();
            }
            Err(e) => {
                assert!(e.contains("Overflow"));
            }
        }
    }
    let _ = writer_handle.join();
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
fn media_packet_layout_hot_fields_in_first_cache_line() {
    // MediaPacket is 56 bytes (Bytes = 32 bytes in bytes-1.12, plus 24 bytes of other fields).
    // ArcInner<MediaPacket> = strong(8) + weak(8) + MediaPacket(56) = 72 bytes.
    //
    // #[repr(C)] with this field order ensures all hot consumer fields (media_type,
    // format, is_keyframe, track_index, pts, dts, payload.ptr, payload.len) land in
    // cache line 0 of the ArcInner (bytes 0–63), so the codec dispatch path never
    // needs a second cache line load.  See the struct-level doc for the full layout.
    assert_eq!(
        std::mem::size_of::<MediaPacket>(),
        56,
        "MediaPacket must be 56 bytes; if this fails, Bytes changed its internal layout"
    );
    // Verify field ordering: media_type must be at offset 0, payload last.
    let p = MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0xDEAD_BEEF,
        pts: 0,
        dts: 0,
        payload: Bytes::new(),
    };
    let base = &p as *const MediaPacket as usize;
    let mt_off = &p.media_type as *const MediaType as usize - base;
    let pl_off = &p.payload as *const Bytes as usize - base;
    assert_eq!(mt_off, 0, "media_type must be the first field (offset 0)");
    assert!(
        pl_off >= 24,
        "payload must be after timestamps (offset >= 24)"
    );
    // The two enums must be exactly 1 byte each.
    assert_eq!(std::mem::size_of::<MediaType>(), 1);
    assert_eq!(std::mem::size_of::<PayloadFormat>(), 1);
}

// ── Fault injection ─────────────────────────────────────────────

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
    rb.push(video_packet(50, 50, false)); // backwards DTS
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
    // Create reader before push so it starts at write_idx=0 and sees all packets.
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
    let mut e = DtsEnforcer::new(1);
    assert_eq!(e.enforce(0, 1_000_000, 1_000_000), (1_000_000, 1_000_000));
    let (pts, dts) = e.enforce(0, 0, 0);
    assert!(dts > 1_000_000, "DTS must be bumped past previous value");
    assert!(pts >= dts, "PTS must be >= DTS");
}

#[test]
fn dts_enforcer_fault_injection_negative_pts_dts() {
    let mut e = DtsEnforcer::new(1);
    let (pts, dts) = e.enforce(0, -100, -100);
    assert_eq!((pts, dts), (-100, -100));
    let (pts2, dts2) = e.enforce(0, -200, -200);
    assert!(dts2 > -100, "DTS must be bumped past -100");
    assert!(pts2 >= dts2);
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
fn active_reader_count_tracks_live_readers() {
    let rb = Arc::new(RingBuffer::new(16));
    assert_eq!(rb.active_reader_count(), 0, "empty ring has no readers");

    let r1 = Reader::new("r1".to_string(), rb.clone());
    assert_eq!(rb.active_reader_count(), 1);

    let r2 = Reader::new("r2".to_string(), rb.clone());
    assert_eq!(rb.active_reader_count(), 2);

    drop(r1);
    // active_reader_count prunes dead Weak refs on each call
    assert_eq!(rb.active_reader_count(), 1);

    drop(r2);
    assert_eq!(rb.active_reader_count(), 0);
}

#[tokio::test]
async fn end_of_stream_wakes_caught_up_reader() {
    let rb = Arc::new(RingBuffer::new(16));
    let mut reader = Reader::new_live("eos-reader".to_string(), rb.clone());

    rb.mark_end_of_stream();
    tokio::time::timeout(std::time::Duration::from_secs(1), reader.wait_for_data())
        .await
        .expect("end-of-stream should wake a caught-up reader");

    assert!(reader.is_caught_up_to_end_of_stream());
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

#[tokio::test]
async fn seal_and_forward_migrates_reader_without_gap() {
    // Simulate adaptive ring resize during live ingest.
    // Writer pushes 5 packets to old ring, seals it, starts new ring.
    // Reader must drain old ring then transparently continue on new ring —
    // no cancel, no reconnect, no missed packets.
    let old_ring = Arc::new(RingBuffer::new(16));
    let mut reader = Reader::new("r".to_string(), old_ring.clone());

    // Push 5 packets to old ring.
    for i in 0i64..5 {
        old_ring.push(video_packet(i * 33, i * 33, i == 0));
    }
    let old_write_idx = old_ring.get_write_idx(); // == 5

    // Create new ring continuing the write index.
    let new_ring = Arc::new(RingBuffer::new_continuing(64, old_write_idx));
    // Seal old ring and link to new ring (mimics adapt_pipeline_ring).
    old_ring.seal_and_forward(new_ring.clone());

    // Push 3 more packets to the NEW ring.
    for i in 5i64..8 {
        new_ring.push(video_packet(i * 33, i * 33, false));
    }

    // Reader must pull all 8 packets with no gaps.
    let mut out = Vec::new();
    let n1 = reader.pull_burst(&mut out, 32).unwrap();
    assert_eq!(n1, 5, "first burst: old ring packets");

    // Now at read_idx == 5 == old write_idx: wait_for_data migrates to new ring.
    // (In a real async context wait_for_data would poll; here we drive it manually.)
    reader.wait_for_data().await;

    let n2 = reader.pull_burst(&mut out, 32).unwrap();
    assert_eq!(n2, 3, "second burst: new ring packets");

    assert_eq!(out.len(), 8, "all 8 packets received without gap");
    // Read index now points into new ring.
    assert_eq!(reader.read_idx, 8);
    // Verify reader's buffer is now the new ring.
    assert_eq!(
        Arc::as_ptr(&reader.buffer),
        Arc::as_ptr(&new_ring),
        "reader migrated to new ring"
    );
}

#[test]
fn continuing_ring_seed_preserves_late_reader_keyframe_preroll() {
    let old_ring = Arc::new(RingBuffer::new(16));
    for i in 0i64..10 {
        old_ring.push(video_packet(i * 33, i * 33, i == 0 || i == 6));
    }

    let old_write_idx = old_ring.get_write_idx();
    let new_ring = Arc::new(RingBuffer::new_continuing(64, old_write_idx));
    let copied = new_ring.seed_readable_tail_from(&old_ring);

    assert_eq!(copied, 10);
    assert_eq!(new_ring.get_write_idx(), old_write_idx);
    assert_eq!(new_ring.fast_forward(old_write_idx), 6);

    let mut late_reader =
        Reader::new_with_keyframe_preroll("late_scaled_stage".to_string(), new_ring, 2);
    let first = late_reader.pull().unwrap().unwrap();
    assert_eq!(
        first.pts,
        4 * 33,
        "preroll starts before the copied keyframe"
    );

    let mut output = vec![first];
    assert_eq!(late_reader.pull_burst(&mut output, 32).unwrap(), 5);
    assert!(
        output
            .iter()
            .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
        "late reader must receive the copied startup keyframe"
    );
}

#[tokio::test]
async fn stage_input_reader_rewinds_to_seeded_keyframe_after_resize_migration() {
    let old_ring = Arc::new(RingBuffer::new(16));
    for i in 0i64..10 {
        old_ring.push(video_packet(i * 33, i * 33, i == 0 || i == 6));
    }

    let old_write_idx = old_ring.get_write_idx();
    let mut reader = Reader::new_stage_input("pre_resize_stage".to_string(), old_ring.clone(), 2);
    reader.read_idx = old_write_idx;
    reader.info.read_idx.store(old_write_idx, Ordering::Relaxed);

    let new_ring = Arc::new(RingBuffer::new_continuing(64, old_write_idx));
    new_ring.seed_readable_tail_from(&old_ring);
    old_ring.seal_and_forward(new_ring);

    reader.wait_for_data().await;
    let first = reader.pull().unwrap().unwrap();
    assert_eq!(
        first.pts,
        4 * 33,
        "stage reader should rewind into seeded keyframe preroll after resize"
    );
}
