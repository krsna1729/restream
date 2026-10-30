//! A shard-wide panic in the real RTMP backend, end to end: split out of
//! `rtmp_shard_tests.rs` to stay under the source-audit line cap.

use super::*;

/// The production startup source, except that asking for a `boom-*`
/// output's startup panics the shard thread: a shard-wide panic in the real
/// RTMP backend, outside any leaf's own panic boundary.
struct PanickingStartupSource(SharedRtmpPublishStartupSource);

impl RtmpPublishStartupSource for PanickingStartupSource {
    fn take_startup(&mut self, output_id: &OutputId) -> Option<RtmpPublishStartup> {
        if output_id.as_str().starts_with("boom") {
            crate::test_support::with_expected_panic_suppressed(|| panic!("scripted shard panic"));
        }
        self.0.take_startup(output_id)
    }
}

/// End to end against a real RTMP peer: an output publishing on a shard
/// whose thread then panics is ended for its application task, the shard
/// is replaced by the next rescale, and the retried output publishes and
/// receives media again, including media published after the replacement
/// (the feed wake reaches the new shard thread).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retried_output_delivers_again_after_its_shard_panics() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("rtmp://{}/live/key", listener.local_addr().unwrap());
    let (publish_tx, publish_rx) = std::sync::mpsc::channel::<()>();
    let (video_tx, video_rx) = std::sync::mpsc::channel::<()>();
    // The output's first attempt, the panicking output, the retry.
    let server = thread::spawn(move || {
        let peers: Vec<_> = (0..3)
            .map(|_| {
                let (stream, _) = listener.accept().unwrap();
                let (publish_tx, video_tx) = (publish_tx.clone(), video_tx.clone());
                thread::spawn(move || {
                    run_accepting_server_peer_reporting_video_after_idle(
                        stream, publish_tx, video_tx,
                    )
                })
            })
            .collect();
        for peer in peers {
            peer.join().unwrap();
        }
    });
    let wait = |rx: &std::sync::mpsc::Receiver<()>, what: &str| {
        rx.recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|_| panic!("{what} never happened"));
    };

    let ring = Arc::new(restream_sync::RingBuffer::new(4));
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let packet = |timestamp| restream_core::MediaPacket {
        media_type: restream_core::MediaType::Video,
        format: restream_core::PayloadFormat::Flv,
        is_keyframe: true,
        track_index: 0,
        pts: timestamp,
        dts: timestamp,
        payload: bytes::Bytes::from_static(&[
            0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x65, 0x88,
        ]),
    };
    ring.push(packet(0));
    let startup_source = SharedRtmpPublishStartupSource::new();
    let shard_config = EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(5)).unwrap();
    let factory_for = |_shard_id| {
        let feed = feed.clone_reader();
        let startup_source = PanickingStartupSource(startup_source.clone());
        move || {
            Ok::<_, TcpEgressPollError>(
                crate::egress::backends::rtmp_shard_resolve_runtime::resolving_rtmp_shard_backend(
                    CompioTcpPoller::new(4)?,
                    feed,
                    budget(),
                    4096,
                    crate::egress::tls::resolve_client_config(None).unwrap(),
                    startup_source,
                    shard_config.drain_timeout(),
                    shard_config.leaf_capacity().get(),
                ),
            )
        }
    };
    let group = crate::egress::shard::EgressShardGroup::try_spawn_with(
        std::num::NonZeroU32::MIN,
        shard_config,
        factory_for,
    )
    .unwrap();
    let mut runtime = crate::egress::runtime::EgressFabricRuntime::new(
        crate::egress::manager::EgressManagerConfig::new(1, 16).unwrap(),
        group,
    )
    .unwrap();
    let wakes = crate::egress::runtime::subscribe_fabric_wakes(
        "rtmp",
        crate::egress::command::FeedId::new("feed"),
        feed.publication_ring().publication_subscribers(),
        runtime.feed_wake_handles(),
    );

    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut output = output_spec("out-1", &url, 1);
    output.progress.terminated_unexpectedly = Some(ended.clone());
    startup_source.set(output.id.clone(), RtmpPublishStartup::default());
    runtime
        .dispatch(EgressCommand::Add(output.clone()))
        .unwrap();
    wait(&publish_rx, "the first publish");
    wait(&video_rx, "the first video");

    runtime
        .dispatch(EgressCommand::Add(output_spec("boom-1", &url, 1)))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !runtime.snapshots()[0].panicked {
        assert!(
            std::time::Instant::now() < deadline,
            "the shard never panicked"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert_eq!(
        runtime.rescale(
            crate::config::EgressShardProfile::OutputCount,
            crate::host_capacity::HostCapacity { cpus: 1 },
            crate::egress::runtime::ResizeReason::Observe,
            shard_config,
            factory_for,
        ),
        Ok(vec![crate::egress::command::ShardId::new(0)])
    );
    assert!(ended.load(std::sync::atomic::Ordering::Acquire));

    // The application task's retry.
    output.generation = 2;
    runtime.dispatch(EgressCommand::Add(output)).unwrap();
    wait(&publish_rx, "the retry's publish");
    wait(&video_rx, "the retry's first video");
    ring.push(packet(33));
    wait(&video_rx, "video published after the replacement");

    drop(wakes);
    runtime.shutdown();
    server.join().unwrap();
}
