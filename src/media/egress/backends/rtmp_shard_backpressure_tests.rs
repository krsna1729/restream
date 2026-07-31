//! Tests for the leaf-level resync-count and feed-lag/backpressure-state
//! observability wiring in `RtmpShardBackend`, split out of
//! `rtmp_shard_tests.rs` to stay under the source-audit line cap.

use super::*;

/// Proves `RtmpShardBackend::resync_count()` (read by
/// `EgressShardRuntime::record_iteration` into `ShardMetrics::feed_resyncs`
/// for the repeated-resync alert, `src/alerts.rs`) actually increments on a
/// real feed overrun, not just on a synthetic `EngineProgress::FeedOverrun`
/// constructed by hand. Pushes more packets than the ring's capacity onto
/// the feed before the leaf's cursor (parked at its initial position since
/// publish acceptance) ever reads one, forcing `feed.read_from` to report
/// `FeedRead::Overrun` on the leaf's next visit.
#[test]
fn resync_count_increments_on_a_real_feed_overrun() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel::<()>();
    let (video_tx, video_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer_reporting_video_after_idle(stream, publish_tx, video_tx);
    });

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    let mut backend = RtmpShardBackend::new(
        TcpEgressPoller::new(4).unwrap(),
        RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new())),
        budget(),
        4096,
    );
    let output_id = OutputId::new("out-1");
    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        &format!("rtmp://{}/live/key", addr),
        1,
    )));
    backend.complete_pending_connect(&output_id, 1, addr);

    let publish_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < publish_deadline,
            "leaf never reached publish acceptance"
        );
        if publish_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(backend.resync_count(), 0);

    // Drive a bit longer so the engine actually reaches its first feed
    // read attempt (and settles into `Interest::NONE`, same as
    // `feed_wake_delivers_media_published_after_the_leaf_goes_idle`)
    // before the overrun is injected below.
    let settle_deadline = std::time::Instant::now() + Duration::from_millis(200);
    while std::time::Instant::now() < settle_deadline {
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    // Push more units than the ring retains (capacity 4) without ever
    // letting the leaf's cursor advance, so its next read sees an overrun.
    let payload = bytes::Bytes::from_static(&[
        0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06,
        0xe2, 0, 0, 0, 1, 0x65, 0x88,
    ]);
    for i in 0..8 {
        ring.push(crate::media::packet::MediaPacket {
            media_type: crate::media::packet::MediaType::Video,
            format: crate::media::packet::PayloadFormat::Raw,
            is_keyframe: true,
            track_index: 0,
            pts: 100 + i,
            dts: 80 + i,
            payload: payload.clone(),
        });
    }
    backend.on_command(EgressCommand::FeedWake);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "feed overrun was never observed by the leaf"
        );
        if backend.resync_count() >= 1 {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }
    assert!(backend.resync_count() >= 1);

    // Let the resynchronized leaf finish delivering whatever it resumed
    // from so the server thread can exit cleanly.
    let drain_deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < drain_deadline && video_rx.try_recv().is_err() {
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    server.join().unwrap();
}

/// Proves `sweep_stalled_leaves` reports every live leaf's feed lag and
/// backpressure classification into `EgressProgressSink` (the cross-thread
/// path feeding `ActiveEgress.feed_lag_units`/`backpressure_reason`, and
/// `feedLagUnits`/`backpressureReason` in the runtime JSON views), not just
/// leaves it decides to force-close. A freshly connected, idle leaf (empty
/// feed, nothing queued) should report zero lag and no backpressure reason.
#[test]
fn sweep_stalled_leaves_reports_feed_lag_and_backpressure_state_for_a_healthy_leaf() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel::<()>();
    let (video_tx, video_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer_reporting_video_after_idle(stream, publish_tx, video_tx);
    });

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(4));
    let mut backend = RtmpShardBackend::new(
        TcpEgressPoller::new(4).unwrap(),
        RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new())),
        budget(),
        4096,
    );
    let output_id = OutputId::new("out-1");
    let feed_lag_units = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let backpressure_reason = Arc::new(std::sync::Mutex::new(Some("unset")));
    backend.on_command(EgressCommand::Add(OutputSpec {
        id: output_id.clone(),
        generation: 1,
        feed: crate::media::egress::command::FeedId::new("feed"),
        protocol: ProtocolSpec::Rtmp {
            url: format!("rtmp://{}/live/key", addr),
            tls: false,
        },
        policy: LeafPolicy::default(),
        progress: EgressProgressSink {
            feed_lag_units: Some(feed_lag_units.clone()),
            backpressure_reason: Some(backpressure_reason.clone()),
            ..Default::default()
        },
    }));
    backend.complete_pending_connect(&output_id, 1, addr);

    let publish_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < publish_deadline,
            "leaf never reached publish acceptance"
        );
        if publish_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    backend.sweep_stalled_leaves(std::time::Instant::now());

    assert_eq!(
        feed_lag_units.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "an idle leaf on an empty feed has no lag"
    );
    assert_eq!(
        *backpressure_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        None,
        "an idle leaf with nothing queued has no backpressure reason"
    );

    // Drive a bit longer so the engine actually reaches its first feed
    // read attempt before the unit below is published, same as
    // `feed_wake_delivers_media_published_after_the_leaf_goes_idle`.
    let settle_deadline = std::time::Instant::now() + Duration::from_millis(200);
    while std::time::Instant::now() < settle_deadline {
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    // Publish one real unit so the server thread observes it and returns
    // cleanly instead of hitting EOF when the test drops the connection.
    let payload = bytes::Bytes::from_static(&[
        0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06,
        0xe2, 0, 0, 0, 1, 0x65, 0x88,
    ]);
    ring.push(crate::media::packet::MediaPacket {
        media_type: crate::media::packet::MediaType::Video,
        format: crate::media::packet::PayloadFormat::Raw,
        is_keyframe: true,
        track_index: 0,
        pts: 100,
        dts: 80,
        payload,
    });
    backend.on_command(EgressCommand::FeedWake);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "published media was never delivered"
        );
        if video_rx.try_recv().is_ok() {
            break;
        }
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    server.join().unwrap();
}
