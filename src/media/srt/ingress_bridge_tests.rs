//! Bridge, sampling and thread-ownership tests for the SRT ingress owner.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use srt_transport::compio::Owner;

use crate::domain::srt_ingest::SrtGlobalIngestConfig;

use super::ingress_admission::ReceiverGroupId;
use super::ingress_owner::{IngressCommand, IngressConfig, SrtIngressEvent, SrtIngressHandle};
use super::tokio_ingress::SrtIngestPolicyStore;

use super::ingress_test_support::*;

// ---------------------------------------------------------------------------
// Bridges and thread ownership
// ---------------------------------------------------------------------------

/// A publisher's media runs to completion on the Owner: with a one-slot
/// lifecycle bridge and this test acting as Tokio (attach on `Connected`,
/// answer the stream probe), every payload the caller sent is demuxed and
/// published, none crosses the bridge, and none is lost.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_runs_publisher_media_to_completion_without_losing_payloads() {
    let entries = vec![plain("sat-key")];
    let store = Arc::new(SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &entries,
    ));
    let stats = Arc::new(crate::media::snapshots::ListenerSocketStats::default());
    let port = free_port();
    let mut handle = SrtIngressHandle::start(IngressConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], port)),
        policy_store: store,
        receiver_group: ReceiverGroupId::generate(),
        stats: stats.clone(),
        command_capacity: 2,
        event_capacity: 1,
        telemetry_capacity: 8,
        owners: 1,
    })
    .await
    .expect("ingress owner starts");
    let remote = handle.local_addr();

    const CHUNKS: usize = 600;
    let chunks = ts_chunks(CHUNKS);
    let sent_bytes = chunks.iter().map(Bytes::len).sum::<usize>() as u64;
    let caller = tokio::task::spawn_blocking(move || {
        run_caller(
            direct(remote, "#!::r=sat-key,m=publish", None),
            Duration::from_secs(30),
            publish_step(chunks),
        )
    });

    let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(8192));
    let (media, bytes_received) = test_publisher_media(ring.clone());
    let mut media = Some(media);
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline
        && bytes_received.load(std::sync::atomic::Ordering::Relaxed) < sent_bytes
    {
        match tokio::time::timeout(Duration::from_millis(100), handle.events.recv()).await {
            Ok(Some(SrtIngressEvent::Connected { logical_peer, .. })) => {
                let command = IngressCommand::AttachPublisher {
                    logical_peer,
                    media: media.take().expect("one publisher"),
                };
                assert!(handle.try_send(command).is_ok());
            }
            Ok(Some(SrtIngressEvent::Probe { logical_peer, .. })) => {
                let command = IngressCommand::ProbeApplied {
                    logical_peer,
                    ring: None,
                };
                assert!(handle.try_send(command).is_ok());
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    let report = caller.await.expect("caller thread");
    assert!(report.connected, "{report:?}");
    assert_eq!(
        bytes_received.load(std::sync::atomic::Ordering::Relaxed),
        sent_bytes,
        "every sent payload was accepted on the Owner"
    );
    assert!(ring.get_write_idx() > 0, "demuxed media reached the ring");
    let snapshot = stats.ingress_owner.snapshot();
    assert_eq!(snapshot.media_payloads, CHUNKS as u64, "{snapshot:?}");
    assert_eq!(snapshot.media_unattached_dropped, 0, "{snapshot:?}");
    let exit = handle.shutdown().await;
    assert!(exit.quiescent, "{exit:?}");
    assert!(exit.fault.is_none());
}

/// Two Owners share the listener port: publishers the kernel hashes to either
/// Owner connect, every session command reaches the Owner named in its
/// `IngressPeer`, every publisher's media is delivered, the listener-wide
/// stats are the sum over both Owners, and an orderly stop withdraws their
/// gauges.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_owners_share_the_port_and_each_session_command_reaches_its_owner() {
    const PUBLISHERS: usize = 24;
    const CHUNKS: usize = 30;
    let entries: Vec<_> = (0..PUBLISHERS)
        .map(|index| plain(&format!("pub-{index}")))
        .collect();
    let store = Arc::new(SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &entries,
    ));
    let stats = Arc::new(crate::media::snapshots::ListenerSocketStats::default());
    let mut handle = SrtIngressHandle::start(IngressConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], free_port())),
        policy_store: store,
        receiver_group: ReceiverGroupId::generate(),
        stats: stats.clone(),
        command_capacity: 64,
        event_capacity: 256,
        telemetry_capacity: 8,
        owners: 2,
    })
    .await
    .expect("two ingress Owners start");
    let remote = handle.local_addr();
    let chunks = ts_chunks(CHUNKS);
    let per_publisher = chunks.iter().map(Bytes::len).sum::<usize>() as u64;
    let callers: Vec<_> = (0..PUBLISHERS)
        .map(|index| {
            let chunks = chunks.clone();
            let stream_id = format!("#!::r=pub-{index},m=publish");
            tokio::task::spawn_blocking(move || {
                run_caller(
                    direct(remote, &stream_id, None),
                    Duration::from_secs(30),
                    publish_step(chunks),
                )
            })
        })
        .collect();

    let mut received = Vec::new();
    let mut owners = std::collections::BTreeSet::new();
    let done = |received: &Vec<Arc<std::sync::atomic::AtomicU64>>| {
        received.len() == PUBLISHERS
            && received
                .iter()
                .all(|bytes| bytes.load(std::sync::atomic::Ordering::Relaxed) == per_publisher)
    };
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline && !done(&received) {
        match tokio::time::timeout(Duration::from_millis(100), handle.events.recv()).await {
            Ok(Some(SrtIngressEvent::Connected { logical_peer, .. })) => {
                owners.insert(logical_peer.owner);
                let ring = Arc::new(crate::media::ring_buffer::RingBuffer::new(1024));
                let (media, bytes) = test_publisher_media(ring);
                received.push(bytes);
                let command = IngressCommand::AttachPublisher {
                    logical_peer,
                    media,
                };
                assert!(handle.try_send(command).is_ok());
            }
            Ok(Some(SrtIngressEvent::Probe { logical_peer, .. })) => {
                let command = IngressCommand::ProbeApplied {
                    logical_peer,
                    ring: None,
                };
                assert!(handle.try_send(command).is_ok());
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    for caller in callers {
        let report = caller.await.expect("caller thread");
        assert!(report.connected, "{report:?}");
    }
    assert!(done(&received), "every publisher's media reached its Owner");
    assert_eq!(
        owners.into_iter().collect::<Vec<_>>(),
        [0, 1],
        "the kernel spread publishers over both Owners"
    );
    let snapshot = stats.ingress_owner.snapshot();
    assert_eq!(
        snapshot.media_payloads,
        (PUBLISHERS * CHUNKS) as u64,
        "{snapshot:?}"
    );
    assert_eq!(snapshot.media_unattached_dropped, 0, "{snapshot:?}");
    assert_eq!(
        snapshot.tx_capacity,
        2 * super::ingress_owner::INGRESS_TX_CAPACITY as u64,
        "each Owner adds its TX capacity"
    );
    let exit = handle.shutdown().await;
    assert!(exit.quiescent, "{exit:?}");
    assert!(exit.fault.is_none());
    let snapshot = stats.ingress_owner.snapshot();
    assert_eq!(
        (snapshot.tx_capacity, snapshot.peers, snapshot.tx_in_flight),
        (0, 0, 0),
        "stopped Owners withdraw their gauges"
    );
}

/// A publisher's Owner-side media state wired to `ring`, plus its received
/// byte counter.
fn test_publisher_media(
    ring: Arc<crate::media::ring_buffer::RingBuffer>,
) -> (
    Box<super::ingress_media::SrtPublisherMedia>,
    Arc<std::sync::atomic::AtomicU64>,
) {
    let bytes_received = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let media = Box::new(super::ingress_media::SrtPublisherMedia {
        registration: crate::media::engine::IngestRegistration {
            cancel_token: tokio_util::sync::CancellationToken::new(),
            attempt_id: 1,
            input_id: "input".to_string(),
            gate: Arc::new(crate::media::input_gate::InputPacketGate::active()),
            last_forwarded_dts: Arc::new(std::sync::atomic::AtomicI64::new(i64::MIN)),
            preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
        },
        ring_buffer: ring,
        demuxer: crate::media::mpegts::TsDemuxer::new(),
        timestamp_mapper: crate::media::input_gate::InputTimestampMapper::default(),
        standby_gop: crate::media::standby_gop::StandbyGopCache::default(),
        packets: Vec::new(),
        keyframe_times: Arc::new(std::sync::Mutex::new(Vec::new())),
        bytes_received: bytes_received.clone(),
        ingest_metrics: Arc::new(crate::media::stage_metrics::StageMetrics::new()),
        last_progress_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        probe_sent: false,
    });
    (media, bytes_received)
}

async fn start_quality_owner(
    key: &str,
    telemetry_capacity: usize,
) -> (
    SrtIngressHandle,
    Arc<crate::media::snapshots::ListenerSocketStats>,
) {
    let entries = vec![plain(key)];
    let store = Arc::new(SrtIngestPolicyStore::new(
        SrtGlobalIngestConfig::default(),
        &entries,
    ));
    let stats = Arc::new(crate::media::snapshots::ListenerSocketStats::default());
    let handle = SrtIngressHandle::start(IngressConfig {
        bind: SocketAddr::from(([127, 0, 0, 1], free_port())),
        policy_store: store,
        receiver_group: ReceiverGroupId::generate(),
        stats: stats.clone(),
        command_capacity: 16,
        event_capacity: 256,
        telemetry_capacity,
        owners: 1,
    })
    .await
    .expect("ingress owner starts");
    (handle, stats)
}

/// Spawn a publisher that stays connected for `stay`, then leaves.
fn stay_connected_publisher(
    remote: SocketAddr,
    stream_id: &'static str,
    stay: Duration,
) -> tokio::task::JoinHandle<CallerReport> {
    tokio::task::spawn_blocking(move || {
        let mut inner = publish_step(ts_chunks(40));
        let started = Instant::now();
        run_caller(
            direct(remote, stream_id, None),
            Duration::from_secs(25),
            move |ctx| {
                let _ = inner(ctx);
                started.elapsed() > stay
            },
        )
    })
}

/// The Owner stamps authoritative receiver samples for a live publisher, they
/// arrive in increasing observation order, and a retired peer is never sampled
/// again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_stamps_receive_quality_and_stops_sampling_retired_peers() {
    let (mut handle, _stats) = start_quality_owner("q-key", 64).await;
    let caller = stay_connected_publisher(
        handle.local_addr(),
        "#!::r=q-key,m=publish",
        Duration::from_secs(4),
    );

    let mut peer = None;
    let mut samples = Vec::new();
    let mut disconnected = false;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !disconnected {
        tokio::select! {
            event = handle.events.recv() => match event {
                Some(SrtIngressEvent::Connected { logical_peer, .. }) => peer = Some(logical_peer),
                Some(SrtIngressEvent::Disconnected { .. }) => disconnected = true,
                Some(_) => {}
                None => break,
            },
            Some(sample) = handle.telemetry.recv() => samples.push(sample),
        }
    }
    let _ = caller.await;
    let peer = peer.expect("the publisher connected");
    assert!(disconnected, "the publisher's disconnect reached Tokio");
    assert!(
        samples.len() >= 2,
        "sampled at least twice: {}",
        samples.len()
    );
    assert!(samples.iter().all(|sample| sample.peer == peer));
    assert!(
        samples[0].observation.sample.max_buffer_packets > 0,
        "capacity is authoritative: {:?}",
        samples[0]
    );
    assert!(samples[0].observation.sample.tsbpd_delay_micros >= 60_000);
    assert!(
        samples
            .windows(2)
            .all(|pair| pair[1].observation.observed_at > pair[0].observation.observed_at),
        "every sample carries a strictly newer Owner observation time"
    );
    // A retired peer is never sampled again.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    let mut after = 0;
    while let Ok(sample) = handle.telemetry.try_recv() {
        if sample.observation.observed_at > samples.last().expect("sampled").observation.observed_at
        {
            after += 1;
        }
    }
    assert_eq!(after, 0, "no samples after the peer retired");
    let exit = handle.shutdown().await;
    assert!(exit.quiescent, "{exit:?}");
}

/// Telemetry is lossy by design: with a one-slot bridge that nobody drains the
/// Owner drops and counts samples instead of stalling, and protocol progress
/// (media delivery) is unaffected.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_telemetry_bridge_drops_samples_and_never_delays_the_protocol() {
    let (mut handle, stats) = start_quality_owner("t-key", 1).await;
    let caller = stay_connected_publisher(
        handle.local_addr(),
        "#!::r=t-key,m=publish",
        Duration::from_secs(4),
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match tokio::time::timeout(Duration::from_millis(200), handle.events.recv()).await {
            Ok(Some(SrtIngressEvent::Disconnected { .. })) => break,
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    let _ = caller.await;
    let snapshot = stats.ingress_owner.snapshot();
    assert!(!snapshot.faulted);
    assert!(
        snapshot.telemetry_dropped >= 1,
        "samples were dropped and counted: {snapshot:?}"
    );
    assert!(
        snapshot.rx_packets >= 40,
        "the protocol kept receiving: {snapshot:?}"
    );
    let exit = handle.shutdown().await;
    assert!(exit.quiescent, "{exit:?}");
}

/// End to end: the publisher's receive quality reaches the ingest snapshot the
/// status API, alerts and diagnostics read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publisher_receive_quality_reaches_the_ingest_snapshot() {
    let server = TestServer::start(&["live-q"], vec![plain("live-q")]).await;
    let remote = server.remote;
    let caller = tokio::task::spawn_blocking(move || {
        let mut inner = publish_step(ts_chunks(60));
        let started = Instant::now();
        run_caller(
            direct(remote, "#!::r=live-q,m=publish", None),
            Duration::from_secs(25),
            move |ctx| {
                let _ = inner(ctx);
                started.elapsed() > Duration::from_secs(10)
            },
        )
    });
    let seen = Arc::new(std::sync::Mutex::new(None));
    server
        .wait_until(
            "publisher receive quality is published",
            Duration::from_secs(15),
            || async {
                let active = server.engine.ingests.active.read().await;
                let found = active
                    .get("pipeline-live-q")
                    .map(|ingest| ingest.metadata().quality)
                    .filter(|quality| quality.srt_recv_buf_capacity_packets.is_some());
                let present = found.is_some();
                if present {
                    *seen.lock().unwrap() = found;
                }
                present
            },
        )
        .await;
    let quality = seen.lock().unwrap().clone().expect("waited for it");
    assert!(quality.srt_recv_buf_capacity_packets.unwrap_or(0) > 0);
    assert!(quality.ms_receive_tsb_pd_delay.unwrap_or(0.0) >= 60.0);
    assert!(quality.ms_rtt.is_some());
    assert!(quality.mbps_receive_rate.is_some());
    let _ = caller.await;
    server.stop().await;
}

/// A bonded publisher's snapshot carries bond identity and member state, keeps
/// leg-level wire degradation explicit, and does not fold it into the ordinary
/// publisher loss counters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bonded_publisher_quality_reports_group_state_not_summed_legs() {
    let server = TestServer::start(&["live-bq"], vec![plain("live-bq")]).await;
    let remote = server.remote;
    let caller = tokio::task::spawn_blocking(move || {
        let mut inner = publish_step(ts_chunks(60));
        let started = Instant::now();
        run_caller(
            bonded(
                remote,
                "#!::r=live-bq,m=publish",
                srt_proto::handshake::GroupType::Broadcast,
            ),
            Duration::from_secs(30),
            move |ctx| {
                let _ = inner(ctx);
                started.elapsed() > Duration::from_secs(12)
            },
        )
    });
    let seen = Arc::new(std::sync::Mutex::new(None));
    server
        .wait_until(
            "bonded publisher quality with a logical rate",
            Duration::from_secs(20),
            || async {
                let active = server.engine.ingests.active.read().await;
                let found = active
                    .get("pipeline-live-bq")
                    .map(|ingest| ingest.metadata().quality)
                    .filter(|quality| {
                        quality.srt_bonded == Some(true) && quality.mbps_receive_rate.is_some()
                    });
                let present = found.is_some();
                if present {
                    *seen.lock().unwrap() = found;
                }
                present
            },
        )
        .await;
    let quality = seen.lock().unwrap().clone().expect("waited for it");
    assert_eq!(quality.srt_group_member_count, Some(2));
    assert!(quality.srt_group_connected_members.unwrap_or(0) >= 1);
    assert!(quality.srt_group_active_members.unwrap_or(0) >= 1);
    assert_eq!(quality.srt_group_broken_members, Some(0));
    assert!(quality.srt_group_wire_receiver_packets_lost.is_some());
    assert!(quality.srt_group_wire_packets_undecryptable.is_some());
    assert_eq!(
        quality.packets_received_loss, None,
        "wire loss is not ordinary publisher loss"
    );
    assert!(quality.srt_recv_buf_capacity_packets.unwrap_or(0) > 0);
    let _ = caller.await;
    server.stop().await;
}

/// The Owner and its runtime are `!Send`: they cannot cross to Tokio.
#[test]
fn owner_and_runtime_are_not_send() {
    trait AmbiguousIfSend<A> {
        fn some_item() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    // Compiles only while `Owner` is NOT `Send` (otherwise this is ambiguous).
    let _ = <Owner as AmbiguousIfSend<_>>::some_item;
}

/// One listener, many peers: exactly one SRT ingress thread serves them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_ingress_owner_thread_serves_many_peers() {
    let keys: Vec<String> = (0..4).map(|index| format!("multi-{index}")).collect();
    let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
    let server = TestServer::start(&key_refs, keys.iter().map(|key| plain(key)).collect()).await;
    let remote = server.remote;
    let mut callers = Vec::new();
    for key in keys.clone() {
        callers.push(tokio::task::spawn_blocking(move || {
            run_caller(
                direct(remote, &format!("#!::r={key},m=publish"), None),
                Duration::from_secs(8),
                {
                    let chunks = ts_chunks(20);
                    let mut inner = publish_step(chunks);
                    move |ctx| inner(ctx)
                },
            )
        }));
    }
    server
        .wait_until(
            "all peers are admitted",
            Duration::from_secs(10),
            || async {
                server
                    .engine
                    .listener_stats_handle()
                    .ingress_owner
                    .snapshot()
                    .peers
                    >= 4
            },
        )
        .await;
    let thread_name = format!("srt-in-{}", remote.port());
    let ingress_threads = std::fs::read_dir("/proc/self/task")
        .expect("task list")
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path().join("comm")).ok())
        .filter(|comm| comm.trim() == thread_name)
        .count();
    assert_eq!(ingress_threads, 1, "one thread serves every peer");
    for caller in callers {
        let _ = caller.await;
    }
    server.stop().await;
}
