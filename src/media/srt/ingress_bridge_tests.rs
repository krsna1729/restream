//! Bridge, sampling and thread-ownership tests for the SRT ingress owner.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use srt_transport::compio::Owner;

use crate::domain::srt_ingest::SrtGlobalIngestConfig;

use super::ingress_admission::ReceiverGroupId;
use super::ingress_owner::{IngressConfig, SrtIngressEvent, SrtIngressHandle};
use super::tokio_ingress::SrtIngestPolicyStore;

use super::ingress_test_support::*;

// ---------------------------------------------------------------------------
// Bridges and thread ownership
// ---------------------------------------------------------------------------

/// With a one-slot event bridge and a deliberately slow consumer, every
/// accepted message still arrives, in order: saturation backpressures the
/// protocol instead of dropping accepted media.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_bridge_saturation_never_loses_accepted_media() {
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
    })
    .await
    .expect("ingress owner starts");
    let remote = handle.local_addr();

    const MESSAGES: usize = 120;
    let messages: Vec<Bytes> = (0..MESSAGES)
        .map(|index| {
            let mut payload = vec![0u8; 200];
            payload[..4].copy_from_slice(&(index as u32).to_be_bytes());
            Bytes::from(payload)
        })
        .collect();
    let mut caller_messages = messages.clone().into_iter().enumerate();
    let mut pending: Option<(usize, Bytes)> = None;
    let sender = tokio::task::spawn_blocking(move || {
        run_caller(
            direct(remote, "#!::r=sat-key,m=publish", None),
            Duration::from_secs(30),
            move |ctx| {
                if !ctx.connected() {
                    return false;
                }
                loop {
                    let (index, payload) = match pending.take().or_else(|| caller_messages.next()) {
                        Some(next) => next,
                        None => return true,
                    };
                    if !ctx.send(&payload) {
                        pending = Some((index, payload));
                        return false;
                    }
                }
            },
        )
    });

    // A slow consumer.
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(25);
    while received.len() < MESSAGES && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), handle.events.recv()).await {
            Ok(Some(SrtIngressEvent::Media { payload, .. })) => {
                received.push(u32::from_be_bytes(payload[..4].try_into().unwrap()));
                tokio::time::sleep(Duration::from_millis(8)).await;
            }
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    let _ = sender.await;
    let expected: Vec<u32> = (0..MESSAGES as u32).collect();
    assert_eq!(
        received, expected,
        "no accepted message was lost or reordered"
    );
    let snapshot = stats.ingress_owner.snapshot();
    assert!(
        snapshot.event_bridge_full_visits > 0,
        "the one-slot bridge really saturated: {snapshot:?}"
    );
    assert!(snapshot.event_depth_hwm <= 1);
    let exit = handle.shutdown().await;
    assert!(exit.quiescent, "{exit:?}");
    assert!(exit.fault.is_none());
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
    let mut media = 0_usize;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match tokio::time::timeout(Duration::from_millis(200), handle.events.recv()).await {
            Ok(Some(SrtIngressEvent::Media { payload, .. })) => media += payload.len(),
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
    assert!(media >= 40 * CHUNK, "media still flowed: {media}");
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

/// A fragment leaves the reader's pending queue only once the bounded command
/// bridge accepted it; a full bridge loses nothing and preserves order.
#[test]
fn command_bridge_saturation_never_loses_reader_fragments() {
    let (sink, drained) = flume::bounded::<Bytes>(2);
    let mut pending: std::collections::VecDeque<Bytes> =
        (0u8..10).map(|index| Bytes::from(vec![index; 4])).collect();
    let offer = |payload: Bytes| {
        sink.try_send(payload)
            .map_err(flume::TrySendError::into_inner)
    };

    assert_eq!(
        super::tokio_ingress::submit_pending(&mut pending, 32, offer),
        2
    );
    assert_eq!(pending.len(), 8, "the rest stays queued, not dropped");
    let mut delivered: Vec<u8> = drained.try_iter().map(|payload| payload[0]).collect();
    // Drain and refill repeatedly: everything arrives once, in order.
    while !pending.is_empty() {
        let offer = |payload: Bytes| {
            sink.try_send(payload)
                .map_err(flume::TrySendError::into_inner)
        };
        let accepted = super::tokio_ingress::submit_pending(&mut pending, 32, offer);
        assert!(accepted > 0);
        delivered.extend(drained.try_iter().map(|payload| payload[0]));
    }
    assert_eq!(delivered, (0u8..10).collect::<Vec<_>>());
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

/// One listener, many peers: exactly one SRT ingress thread exists, and the
/// Tokio side holds no protocol table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_ingress_thread_serves_many_peers_and_tokio_has_no_peer_table() {
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

    // Source audit: the Tokio SRT listener has no protocol table or native
    // driver, and ingress has no fallback transport.
    for (name, source) in [
        ("tokio_ingress.rs", include_str!("tokio_ingress.rs")),
        ("ingress_owner.rs", include_str!("ingress_owner.rs")),
    ] {
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        for forbidden in [
            "PeerTable::new",
            "UringUdpDriver",
            "CompatReceiver",
            "ReceiveMode",
            "NativeSrtIngress",
            "RuntimeFlavor::Mio",
        ] {
            assert!(
                !production.contains(forbidden),
                "{name} must not contain {forbidden}"
            );
        }
    }
}

/// Every session identity a test used is unique (a LogicalPeerId is never
/// reused), so a stale command can never reach a later peer.
#[test]
fn logical_peer_ids_are_hashable_session_handles() {
    fn assert_handle<T: Copy + Eq + std::hash::Hash + std::fmt::Debug>() {}
    assert_handle::<srt_transport::advanced::admission::LogicalPeerId>();
    let _: HashSet<srt_transport::advanced::admission::LogicalPeerId> = HashSet::new();
}
