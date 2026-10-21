//! Live tests of the SRT ingress owner: a real `srt_transport::compio::Owner`
//! listener on a real UDP socket, driven by a real srt-rs caller (direct and
//! bonded) on its own thread, feeding the real Restream session/media path.
//!
//! These prove the properties only a real Owner session can: asynchronous
//! rejection and pipeline deletion actually disconnect the protocol peer, SRT
//! read/play sends through the Owner's logical peer, bonded Broadcast/Backup
//! ingest reaches Tokio as one logical peer without duplicated application
//! bytes, and the thread bridges never silently lose accepted media.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use srt_proto::crypto::KeyLength;
use srt_proto::handshake::GroupType;

use crate::media::ingest_auth::AuthenticatedPipeline;

use super::ingress_test_support::*;

// ---------------------------------------------------------------------------
// Publish (direct, plaintext / encrypted / refused)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_publisher_reaches_the_pipeline_ring() {
    let server = TestServer::start(&["live-plain"], vec![plain("live-plain")]).await;
    let remote = server.remote;
    let chunks = ts_chunks(400);
    let report = blocking(move || {
        run_caller(
            direct(remote, "#!::r=live-plain,m=publish", None),
            Duration::from_secs(20),
            publish_step(chunks),
        )
    })
    .await;
    assert!(report.connected, "{report:?}");
    assert!(report.sent_bytes >= 100 * CHUNK, "{report:?}");
    assert!(
        server.ring_progress("pipeline-live-plain").await > 0,
        "media reached the pipeline ring through the Owner"
    );
    let stats = server
        .engine
        .listener_stats_handle()
        .ingress_owner
        .snapshot();
    assert!(stats.rx_packets > 0 && stats.tx_packets > 0, "{stats:?}");
    assert!(!stats.faulted);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_stream_publishes_and_wrong_or_unknown_credentials_do_not() {
    let server = TestServer::start(
        &["enc-key"],
        vec![encrypted("enc-key", "correct-passphrase-1", 16)],
    )
    .await;
    let remote = server.remote;

    // Correct passphrase publishes.
    let chunks = ts_chunks(300);
    let good = blocking(move || {
        run_caller(
            direct(
                remote,
                "#!::r=enc-key,m=publish",
                Some(("correct-passphrase-1", KeyLength::Aes128)),
            ),
            Duration::from_secs(20),
            publish_step(chunks),
        )
    })
    .await;
    assert!(good.connected, "{good:?}");
    assert!(server.ring_progress("pipeline-enc-key").await > 0);

    // Wrong passphrase, plaintext against an encrypted policy, and an unknown
    // stream never become application publishers.
    let attempts: Vec<(&str, Option<(&str, KeyLength)>)> = vec![
        (
            "#!::r=enc-key,m=publish",
            Some(("wrong-passphrase-99", KeyLength::Aes128)),
        ),
        ("#!::r=enc-key,m=publish", None),
        (
            "#!::r=unknown-key,m=publish",
            Some(("correct-passphrase-1", KeyLength::Aes128)),
        ),
    ];
    for (stream_id, crypto) in attempts {
        let stream_id = stream_id.to_string();
        let report = blocking(move || {
            run_caller(
                direct(remote, &stream_id, crypto),
                Duration::from_millis(1500),
                |_| false,
            )
        })
        .await;
        assert!(!report.connected, "must not connect: {report:?}");
    }
    let snapshot = server
        .engine
        .listener_stats_handle()
        .ingress_owner
        .snapshot();
    assert!(
        snapshot.policy_rejections >= 1 || snapshot.credential_failures >= 1,
        "the refusals are observable: {snapshot:?}"
    );
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Asynchronous authorization rejection and pipeline deletion
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asynchronous_rejection_disconnects_the_owner_peer() {
    // The policy store accepts the key at handshake time, but pipeline
    // authentication (the asynchronous step) refuses it.
    let server = TestServer::start(&[], vec![plain("async-reject")]).await;
    let remote = server.remote;
    let report = blocking(move || {
        run_caller(
            direct(remote, "#!::r=async-reject,m=publish", None),
            Duration::from_secs(4),
            |ctx| ctx.report.disconnected,
        )
    })
    .await;
    assert!(
        report.connected,
        "the protocol handshake succeeded: {report:?}"
    );
    let (Some(up), Some(down)) = (report.connected_at, report.disconnected_at) else {
        panic!("the Owner peer was never disconnected: {report:?}");
    };
    assert!(
        down.duration_since(up) < Duration::from_secs(3),
        "disconnected promptly, not by idle timeout: {report:?}"
    );
    assert!(!server.ingest_active("pipeline-async-reject").await);
    server
        .wait_until(
            "the Owner retires the peer",
            Duration::from_secs(3),
            || async {
                server
                    .engine
                    .listener_stats_handle()
                    .ingress_owner
                    .snapshot()
                    .peers
                    == 0
            },
        )
        .await;
    server.stop().await;
}

/// The real product read path against a deterministic active pipeline: an
/// SRT reader connects to the Owner listener, `SrtServer` pulls the shared
/// muxer through `TsChunkReader`, sends over the bounded bridge to the Owner's
/// logical peer, and target deletion produces a protocol disconnect.
///
/// The active pipeline is a fixture (a registered ingest fed with the checked
/// MPEG-TS fixture), not a second live SRT publisher: the publisher path has
/// its own test, and a second caller runtime only adds host contention.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_play_sends_through_the_owner_and_target_deletion_disconnects() {
    const PIPELINE: &str = "pipeline-live-rw";
    let server = TestServer::start(&["live-rw"], vec![plain("live-rw")]).await;
    let remote = server.remote;

    // Fixture: an active publisher built through the production publisher
    // path (`start_publisher` registration, the Owner-side `IngressMedia`
    // demux/gate/publish, and `apply_probe` metadata and ring adaptation),
    // fed the checked MPEG-TS fixture without a second SRT caller.
    let authenticated = AuthenticatedPipeline {
        id: PIPELINE.to_string(),
        input_id: "input-live-rw".to_string(),
        selected: true,
    };
    let (session, publisher_media) = server
        .server
        .start_publisher(
            SocketAddr::from(([127, 0, 0, 1], 9)),
            authenticated,
            "live-rw".to_string(),
        )
        .await
        .expect("the fixture publisher registers");
    assert!(server.ingest_active(PIPELINE).await);

    // Keep the pipeline live for the whole test (about 15 s of paced media);
    // the reader attaches at the live edge whenever it connects.
    let feeder_server = server.server.clone();
    let feeder = tokio::spawn(async move {
        let stats = crate::media::snapshots::SrtIngressOwnerStats::default();
        let mut media = super::ingress_media::IngressMedia::<u64>::default();
        let _ = media.attach_publisher(0, publisher_media, &stats);
        for chunk in ts_chunks(5000) {
            if let Some(probe) = media.on_payload(0, chunk, &stats) {
                let identity = media.probe_identity(0).unwrap();
                let ring = feeder_server.apply_probe(&session, identity, probe).await;
                media.probe_applied(0, identity, ring, &stats);
            }
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
        media
    });

    let engine = server.engine.clone();
    let reader = blocking(move || {
        let mut deleted = false;
        run_caller(
            direct(remote, "#!::r=live-rw,m=request", None),
            Duration::from_secs(40),
            move |ctx| {
                if !deleted && ctx.report.received.len() >= 200 * CHUNK {
                    deleted = true;
                    // Target deletion: the pipeline ring disappears.
                    engine.ingests.pipelines.blocking_write().remove(PIPELINE);
                }
                ctx.report.disconnected
            },
        )
    })
    .await;
    feeder.abort();
    // The feeder owned the publisher session; the deletion above already ended
    // the pipeline, so there is nothing further to unregister.

    let diagnostics = server.diagnostics().await;
    assert!(reader.connected, "{reader:?}; {diagnostics}");
    assert!(
        reader.received.len() >= 200 * CHUNK,
        "the reader received progressing bytes: {} ({reader:?}); {diagnostics}",
        reader.received.len()
    );
    assert_eq!(reader.received[0], 0x47, "MPEG-TS sync byte");
    assert!(
        reader.disconnected,
        "target deletion produced a protocol disconnect: {reader:?}; {diagnostics}"
    );
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Bonded ingest
// ---------------------------------------------------------------------------

async fn bonded_ingest(mode: GroupType) {
    let key = match mode {
        GroupType::Broadcast => "bond-broadcast",
        _ => "bond-backup",
    };
    let server = TestServer::start(&[key], vec![plain(key)]).await;
    let remote = server.remote;
    let stream_id = format!("#!::r={key},m=publish");
    let chunks = ts_chunks(120);
    let expected: usize = chunks.iter().map(Bytes::len).sum();
    let report = blocking(move || {
        run_caller(
            bonded(remote, &stream_id, mode),
            Duration::from_secs(20),
            publish_step(chunks),
        )
    })
    .await;
    assert!(report.connected, "{mode:?}: {report:?}");
    assert_eq!(
        report.group_faults, 0,
        "{mode:?}: the responder identity is one receiving group"
    );
    if mode == GroupType::Broadcast {
        assert_eq!(report.max_active_legs, 2, "{mode:?}: both legs active");
    }
    assert_eq!(report.sent_bytes, expected, "{mode:?}");

    // One logical input at the application: the bytes accounted equal the
    // bytes sent ONCE, never duplicated across legs.
    let pipeline = format!("pipeline-{key}");
    // The same bytes demuxed once give the packet count a single logical
    // input produces; a duplicated application stream would double it.
    let mut demuxer = crate::media::mpegts::TsDemuxer::new();
    let mut once = Vec::new();
    for chunk in ts_chunks(120) {
        demuxer.feed(chunk.as_ref());
    }
    demuxer.flush();
    demuxer.drain_into(&mut once);
    let single_stream_packets = once.len();
    assert!(single_stream_packets > 0);
    let mut progressed = 0;
    for _ in 0..100 {
        progressed = server.ring_progress(&pipeline).await;
        if progressed > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(progressed > 0, "{mode:?}: media reached the pipeline ring");
    assert!(
        progressed <= single_stream_packets,
        "{mode:?}: {progressed} ring packets exceed the {single_stream_packets} one stream yields: \
         Broadcast legs must not duplicate application media"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn broadcast_bond_ingests_as_one_logical_input() {
    bonded_ingest(GroupType::Broadcast).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_bond_ingests_as_one_logical_input() {
    bonded_ingest(GroupType::Backup).await;
}

// ---------------------------------------------------------------------------
// Fault domain: one peer's media panic
// ---------------------------------------------------------------------------

/// A panic in one publisher's media work (demux, here injected) disconnects
/// that publisher only. The Owner keeps running: it is not faulted, and the
/// next publisher on the same listener reaches its ring. Before the
/// per-peer boundary the unwind ended the Owner thread, which stopped the
/// listener and shut Restream down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_publisher_is_disconnected_and_the_owner_keeps_serving() {
    let server = TestServer::start(
        &["doomed", "healthy"],
        vec![plain("doomed"), plain("healthy")],
    )
    .await;
    let remote = server.remote;
    let before = crate::panic_boundary::contained_panics();

    let mut poison = vec![0u8; CHUNK];
    poison[..super::ingress_media::INJECTED_PANIC_PAYLOAD.len()]
        .copy_from_slice(super::ingress_media::INJECTED_PANIC_PAYLOAD);
    let poison = vec![Bytes::from(poison); 50];
    let doomed = blocking(move || {
        let mut publish = publish_step(poison);
        run_caller(
            direct(remote, "#!::r=doomed,m=publish", None),
            Duration::from_secs(6),
            move |ctx| ctx.report.disconnected || publish(ctx),
        )
    })
    .await;
    assert!(doomed.connected, "{doomed:?}");
    assert!(
        doomed.disconnected,
        "the panicking peer is disconnected: {doomed:?}"
    );
    assert!(crate::panic_boundary::contained_panics() > before);

    let chunks = ts_chunks(400);
    let healthy = blocking(move || {
        run_caller(
            direct(remote, "#!::r=healthy,m=publish", None),
            Duration::from_secs(20),
            publish_step(chunks),
        )
    })
    .await;
    assert!(healthy.connected, "{healthy:?}");
    assert!(server.ring_progress("pipeline-healthy").await > 0);
    let stats = server
        .engine
        .listener_stats_handle()
        .ingress_owner
        .snapshot();
    assert!(!stats.faulted, "{stats:?}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragmented_publisher_panic_retires_tail_without_stalling_an_existing_sibling() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    let server = TestServer::start(
        &["fragmented-doomed", "fragmented-healthy"],
        vec![plain("fragmented-doomed"), plain("fragmented-healthy")],
    )
    .await;
    let remote = server.remote;
    let finished = Arc::new(AtomicBool::new(false));
    let sent_after_retirement = Arc::new(AtomicU64::new(0));
    let stop = Arc::clone(&finished);
    let sent = Arc::clone(&sent_after_retirement);
    let healthy = tokio::spawn(blocking(move || {
        let chunks = ts_chunks(512);
        let mut next = 0;
        run_caller(
            direct(remote, "#!::r=fragmented-healthy,m=publish", None),
            Duration::from_secs(30),
            move |ctx| {
                for _ in 0..8 {
                    if !ctx.connected() || !ctx.send(&chunks[next % chunks.len()]) {
                        break;
                    }
                    next += 1;
                    if stop.load(Ordering::Acquire) {
                        sent.fetch_add(1, Ordering::Relaxed);
                    }
                }
                sent.load(Ordering::Relaxed) >= 512
            },
        )
    }));
    server
        .wait_until(
            "healthy publisher reaches its ring",
            Duration::from_secs(10),
            || async { server.ring_progress("pipeline-fragmented-healthy").await > 0 },
        )
        .await;
    let mut message = Vec::with_capacity(4096 * CHUNK);
    let mut poison = vec![0u8; CHUNK];
    poison[..super::ingress_media::INJECTED_PANIC_PAYLOAD.len()]
        .copy_from_slice(super::ingress_media::INJECTED_PANIC_PAYLOAD);
    message.extend_from_slice(&poison);
    for chunk in ts_chunks(512).iter().cycle().take(4095) {
        message.extend_from_slice(chunk);
    }
    let doomed = blocking(move || {
        run_fragmented_publisher(
            remote,
            "#!::r=fragmented-doomed,m=publish",
            &message,
            Duration::from_secs(15),
        )
    })
    .await;
    // The doomed publisher's retirement is the boundary: progress made before
    // it says nothing about the sibling that has to survive it.
    let before_retirement = server.ring_progress("pipeline-fragmented-healthy").await;
    finished.store(true, Ordering::Release);
    server
        .wait_until(
            "the healthy publisher keeps delivering after the doomed peer retired",
            Duration::from_secs(15),
            || async {
                server.ring_progress("pipeline-fragmented-healthy").await > before_retirement
            },
        )
        .await;
    let healthy = healthy.await.expect("healthy publisher task");
    let stats = server
        .engine
        .listener_stats_handle()
        .ingress_owner
        .snapshot();
    assert!(
        doomed.connected && doomed.disconnected,
        "{doomed:?}; Owner: {stats:?}"
    );
    assert_eq!(doomed.sent_bytes, 4096 * CHUNK);
    assert!(healthy.connected && !healthy.disconnected, "{healthy:?}");
    let sent_after = sent_after_retirement.load(Ordering::Relaxed);
    assert!(
        sent_after >= 512,
        "the healthy publisher keeps sending past the retirement boundary \
         (delivery itself is the ring-progress wait above): {sent_after}"
    );
    assert_eq!(
        stats.owners, 1,
        "the fault and sibling must share one ingress Owner"
    );
    assert!(!stats.faulted, "{stats:?}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn q098_stale_control_probe_cannot_modify_a_replacement_publisher() {
    use super::ingress_media::IngressMedia;
    use crate::media::snapshots::SrtIngressOwnerStats;

    const PIPELINE: &str = "pipeline-probe-identity";
    let server = TestServer::start(&["probe-identity"], vec![plain("probe-identity")]).await;
    let authenticated = || AuthenticatedPipeline {
        id: PIPELINE.to_string(),
        input_id: "input-probe-identity".to_string(),
        selected: true,
    };
    let start = || {
        server.server.start_publisher(
            SocketAddr::from(([127, 0, 0, 1], 9)),
            authenticated(),
            "probe-identity".to_string(),
        )
    };
    let stats = SrtIngressOwnerStats::default();
    let (old_session, old_media) = start().await.unwrap();
    let mut media = IngressMedia::<u64>::default();
    media.attach_publisher(1, old_media, &stats);
    let mut old_probe = None;
    for chunk in topology_chunks(false, true, 0) {
        if let Some(probe) = media.on_payload(1, chunk, &stats) {
            old_probe = Some(probe);
        }
    }
    let old_identity = media.probe_identity(1).unwrap();
    server.server.finish_publisher(old_session).await;
    media.discard(1);

    let (replacement, replacement_media) = start().await.unwrap();
    media.attach_publisher(2, replacement_media, &stats);
    let mut replacement_probe = None;
    for chunk in topology_chunks(true, true, 0) {
        if let Some(probe) = media.on_payload(2, chunk, &stats) {
            replacement_probe = Some(probe);
        }
    }
    let identity = media.probe_identity(2).unwrap();
    assert_ne!(identity.attempt_id, old_identity.attempt_id);
    let ring = server
        .server
        .apply_probe(&replacement, identity, replacement_probe.unwrap())
        .await;
    media.probe_applied(2, identity, ring, &stats);
    let before = server.engine.ingests.pipelines.read().await[PIPELINE].clone();
    assert_eq!(before.codec_hint_str(), "h264");

    assert!(
        server
            .server
            .apply_probe(&replacement, old_identity, old_probe.unwrap())
            .await
            .is_none()
    );
    assert_eq!(
        before.codec_hint_str(),
        "h264",
        "stale control cannot remove video"
    );
    assert_eq!(
        server
            .engine
            .with_active_ingest(PIPELINE, |ingest| ingest.metadata().video.is_some())
            .await,
        Some(true),
    );
    server.server.finish_publisher(replacement).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn q098_review_superseded_resize_ack_keeps_current_ring_through_deselection() {
    use super::ingress_media::IngressMedia;
    use crate::media::engine::MediaEngine;
    use crate::media::metadata::VideoMeta;
    use crate::media::snapshots::SrtIngressOwnerStats;
    use std::sync::Arc;

    const PIPELINE: &str = "pipeline-review-handoff";
    let engine = Arc::new(MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        ring_capacity: 8,
        ring_headroom_secs: 5.0,
        ..Default::default()
    })));
    let server = TestServer::start_with_engine(
        &["review-handoff"],
        vec![plain("review-handoff")],
        engine.clone(),
    )
    .await;
    let (session, source) = server
        .server
        .start_publisher(
            SocketAddr::from(([127, 0, 0, 1], 9)),
            AuthenticatedPipeline {
                id: PIPELINE.into(),
                input_id: "primary".into(),
                selected: true,
            },
            "review-handoff".into(),
        )
        .await
        .unwrap();
    let original = engine.ingests.pipelines.read().await[PIPELINE].clone();
    let stats = SrtIngressOwnerStats::default();
    let mut media = IngressMedia::<u64>::default();
    media.attach_publisher(1, source, &stats);
    let mut first = None;
    for chunk in topology_chunks(true, true, 0) {
        if let Some(probe) = media.on_payload(1, chunk, &stats) {
            first = Some(probe);
        }
    }
    let first_identity = media.probe_identity(1).unwrap();
    let first_handoff = server
        .server
        .apply_probe(&session, first_identity, first.unwrap())
        .await;
    let replacement = engine.ingests.pipelines.read().await[PIPELINE].clone();
    assert!(
        !Arc::ptr_eq(&original, &replacement),
        "the first probe must resize"
    );
    let mut second = None;
    for chunk in topology_chunks(false, true, 1) {
        if let Some(probe) = media.on_payload(1, chunk, &stats) {
            second = Some(probe);
        }
    }
    let second = second.unwrap();
    let second_identity = media.probe_identity(1).unwrap();
    let second_handoff = server
        .server
        .apply_probe(&session, second_identity, second.clone())
        .await;
    assert!(
        Arc::ptr_eq(second_handoff.as_ref().unwrap(), &replacement),
        "a no-resize response must still carry the current shared ring"
    );
    let old_write = original.get_write_idx();
    let new_write = replacement.get_write_idx();
    media.probe_applied(1, first_identity, first_handoff, &stats);
    assert_eq!(
        replacement.get_write_idx(),
        new_write,
        "superseded ack cannot release held media"
    );
    media.probe_applied(1, second_identity, second_handoff, &stats);
    assert_eq!(original.get_write_idx(), old_write);
    assert!(
        replacement.get_write_idx() > new_write,
        "held audio reaches readers' current ring"
    );

    let backup = engine
        .try_register_pipeline_input_attempt(PIPELINE, "backup", "backup-key", "srt", false)
        .await
        .unwrap();
    assert!(engine.select_pipeline_input(PIPELINE, "backup").await);
    engine
        .replace_ingest_session_tracks(
            &backup,
            Some(VideoMeta {
                codec: "h264".into(),
                fps: 600.0,
                ..Default::default()
            }),
            Vec::new(),
            1,
            None,
        )
        .await;
    let current = engine.ingests.pipelines.read().await[PIPELINE].clone();
    assert!(!Arc::ptr_eq(&replacement, &current));
    let standby_handoff = server
        .server
        .apply_probe(&session, second_identity, second)
        .await;
    assert!(
        Arc::ptr_eq(standby_handoff.as_ref().unwrap(), &current),
        "a deselected session must not retain a retired producer ring"
    );
    assert_eq!(
        current.codec_hint_str(),
        "h264",
        "standby metadata cannot alter the selected source"
    );
    media.probe_applied(1, second_identity, standby_handoff, &stats);
    server.server.finish_publisher(session).await;
    server.stop().await;
}
