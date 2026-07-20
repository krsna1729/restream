use super::*;

#[test]
fn reader_drop_removes_entry_from_readers_list() {
    let rb = Arc::new(RingBuffer::new(16));

    assert_eq!(
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        0
    );

    let first = Reader::new("r1".into(), rb.clone());
    let second = Reader::new("r2".into(), rb.clone());
    assert_eq!(
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        2
    );

    drop(first);
    assert_eq!(
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );

    drop(second);
    assert_eq!(
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        0
    );
}

#[test]
fn reader_drop_cleans_up_on_poisoned_mutex() {
    let rb = Arc::new(RingBuffer::new(16));
    let reader = Reader::new("r".into(), rb.clone());

    let poisoned_ring = rb.clone();
    let _panic_hook_lock = EXPECTED_PANIC_HOOK_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut panic_hook = ScopedSilentPanicHook::new();
    panic_hook.silence();
    let poison_thread = std::thread::spawn(move || {
        let _guard = poisoned_ring.readers.lock().unwrap();
        panic!("intentional poison");
    });
    let _ = poison_thread.join();

    assert!(rb.readers.lock().is_err());
    drop(reader);
    assert_eq!(
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        0
    );
}

#[test]
fn reader_drop_also_prunes_other_stale_weaks() {
    let rb = Arc::new(RingBuffer::new(16));
    {
        let stale = Arc::new(ReaderInfo::new("stale".into(), 0));
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(Arc::downgrade(&stale));
    }
    assert_eq!(
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        1
    );

    let reader = Reader::new("live".into(), rb.clone());
    assert_eq!(
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        2
    );

    drop(reader);
    assert_eq!(
        rb.readers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len(),
        0
    );
}

#[test]
fn test_concurrent_writer_reader_no_corruption() {
    let rb = Arc::new(RingBuffer::new(4));
    rb.push(video_packet(0, 0, true));
    let mut reader = Reader::new("r1".into(), rb.clone());

    let writer_ring = rb.clone();
    let writer = std::thread::spawn(move || {
        for index in 1..1000 {
            writer_ring.push(video_packet(index * 10, index * 10, index % 10 == 0));
            std::thread::yield_now();
        }
    });

    for _ in 0..2000 {
        match reader.pull() {
            Ok(Some(packet)) => assert!(packet.pts >= 0),
            Ok(None) => std::thread::yield_now(),
            Err(error) => assert!(error.contains("Overflow")),
        }
    }
    let _ = writer.join();
}

#[test]
fn active_reader_count_tracks_live_readers() {
    let rb = Arc::new(RingBuffer::new(16));
    assert_eq!(rb.active_reader_count(), 0, "empty ring has no readers");

    let first = Reader::new("r1".to_string(), rb.clone());
    assert_eq!(rb.active_reader_count(), 1);

    let second = Reader::new("r2".to_string(), rb.clone());
    assert_eq!(rb.active_reader_count(), 2);

    drop(first);
    assert_eq!(rb.active_reader_count(), 1);

    drop(second);
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

#[tokio::test]
async fn seal_and_forward_migrates_reader_without_gap() {
    let old_ring = Arc::new(RingBuffer::new(16));
    let mut reader = Reader::new("r".to_string(), old_ring.clone());

    for index in 0i64..5 {
        old_ring.push(video_packet(index * 33, index * 33, index == 0));
    }
    let old_write_idx = old_ring.get_write_idx();

    let new_ring = Arc::new(RingBuffer::new_continuing(64, old_write_idx));
    old_ring.seal_and_forward(new_ring.clone());

    for index in 5i64..8 {
        new_ring.push(video_packet(index * 33, index * 33, false));
    }

    let mut output = Vec::new();
    let old_count = reader.pull_burst(&mut output, 32).unwrap();
    assert_eq!(old_count, 5, "first burst: old ring packets");

    reader.wait_for_data().await;

    let new_count = reader.pull_burst(&mut output, 32).unwrap();
    assert_eq!(new_count, 3, "second burst: new ring packets");
    assert_eq!(output.len(), 8, "all 8 packets received without gap");
    assert_eq!(reader.read_idx, 8);
    assert_eq!(
        Arc::as_ptr(&reader.buffer),
        Arc::as_ptr(&new_ring),
        "reader migrated to new ring"
    );
}

#[test]
fn continuing_ring_seed_preserves_late_reader_keyframe_preroll() {
    let old_ring = Arc::new(RingBuffer::new(16));
    for index in 0i64..10 {
        old_ring.push(video_packet(
            index * 33,
            index * 33,
            index == 0 || index == 6,
        ));
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
    assert_eq!(first.pts, 4 * 33, "preroll starts before copied keyframe");

    let mut output = vec![first];
    assert_eq!(late_reader.pull_burst(&mut output, 32).unwrap(), 5);
    assert!(
        output
            .iter()
            .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
        "late reader must receive copied startup keyframe"
    );
}

#[tokio::test]
async fn stage_input_reader_rewinds_to_seeded_keyframe_after_resize_migration() {
    let old_ring = Arc::new(RingBuffer::new(16));
    for index in 0i64..10 {
        old_ring.push(video_packet(
            index * 33,
            index * 33,
            index == 0 || index == 6,
        ));
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
