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

    // Fixture: an active publisher session built through the production
    // publisher path (`start_publisher` + `accept_payload`: registration, demux,
    // probe metadata, ring adaptation), fed the checked MPEG-TS fixture without
    // a second SRT caller.
    let authenticated = AuthenticatedPipeline {
        id: PIPELINE.to_string(),
        input_id: "input-live-rw".to_string(),
        selected: true,
    };
    let mut publisher = server
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
    let feeder_engine = server.engine.clone();
    let feeder = tokio::spawn(async move {
        for chunk in ts_chunks(5000) {
            publisher.accept_payload(&feeder_engine, chunk).await;
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
        publisher
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
