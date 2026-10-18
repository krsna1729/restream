//! The listener thread: absent unless configured; a real publisher routed
//! through it into a registered `moq` input; shutdown stops and joins it.

use std::sync::atomic::{AtomicBool, Ordering};

use bytes::{BufMut, Bytes, BytesMut};

use super::*;
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessError, PipelineAccessFuture, PipelineAccessMode,
};
use crate::media::moq::egress::media::HangFrame;
use crate::media::moq::egress::publish::HangBroadcast;
use crate::media::moq::media::{
    AudioConfig, TrackContainer, TrackKind, hang_catalog, write_legacy_prefix,
};
use crate::media::moq::transport::test_support::{
    LOOPBACK_CERT, LOOPBACK_SERVER_NAME, loopback_client_tls, run,
};
use crate::media::srt::AddressFamily;

const INPUT_ID: &str = "input-moq";

/// Accepts stream key `moq-key` for MoQ publish only.
struct OneKey;

impl PipelineAccessAuthenticator for OneKey {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            if mode == PipelineAccessMode::MoqPublish && stream_key == "moq-key" {
                Ok(AuthenticatedPipeline {
                    id: "pipe-moq".to_string(),
                    input_id: INPUT_ID.to_string(),
                    selected: true,
                })
            } else {
                Err(PipelineAccessError::InvalidStreamKey)
            }
        })
    }
}

fn security() -> Arc<IngestSecurityService> {
    Arc::new(IngestSecurityService::new(IngestSecurityConfig::default()))
}

#[tokio::test]
async fn nothing_runs_when_moq_is_off_or_invalid() {
    let engine = Arc::new(MediaEngine::new());
    for setting in [
        MoqIngestSetting::Off,
        MoqIngestSetting::Invalid("RESTREAM_MOQ_TLS_KEY is missing".to_string()),
    ] {
        launch(&setting, Arc::new(OneKey), engine.clone(), security()).await;
    }
    assert!(engine.drain_os_thread_handles().is_empty());
}

#[tokio::test]
async fn an_unreadable_certificate_fails_the_start_and_leaves_nothing_running() {
    let engine = Arc::new(MediaEngine::new());
    let config = MoqIngestConfig {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        tls_cert_path: "/nonexistent/moq-cert.pem".to_string(),
        tls_key_path: "/nonexistent/moq-key.pem".to_string(),
        max_connections: 4,
        origin_max_viewers: 4,
    };
    let server = Arc::new(MoqIngestServer::new(
        Arc::new(OneKey),
        engine.clone(),
        security(),
    ));
    let error = start(&config, server).await.unwrap_err();
    assert!(error.contains("moq-cert.pem"), "{error}");
    assert!(engine.drain_os_thread_handles().is_empty());
}

/// Publish key `moq-key` (one AAC track) to `addr` until `done`.
fn publish_until(addr: SocketAddr, done: Arc<AtomicBool>) {
    run(async move {
        let endpoint = transport::client_endpoint(AddressFamily::V4).await.unwrap();
        let tls = loopback_client_tls(transport::CLIENT_ALPNS);
        let session = transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls)
            .await
            .unwrap();
        let (origin, origin_driver) =
            moq_net::origin::Producer::new(moq_net::origin::Config::default());
        compio::runtime::spawn(transport::run_driver(origin_driver)).detach();
        let mut broadcast = HangBroadcast::create(&origin, "moq-key", false, true).unwrap();
        let audio = AudioConfig {
            sample_rate: 48_000,
            channels: 2,
            asc: Some(Bytes::from_static(&[0x11, 0x90])),
            container: TrackContainer::Legacy,
        };
        broadcast
            .publish_catalog(&hang_catalog(None, Some(&audio)).unwrap())
            .unwrap();
        let (_session, driver) = moq_net::Client::new()
            .with_publisher(&origin)
            .connect_lite(Instant::now(), session)
            .await
            .unwrap();
        compio::runtime::spawn(transport::run_driver(driver)).detach();
        let mut pts_us = 0;
        while !done.load(Ordering::Acquire) {
            let mut data = BytesMut::new();
            write_legacy_prefix(pts_us, &mut data).unwrap();
            data.put_slice(&[0x21, 0x10, 0x05]);
            let frame = HangFrame {
                track: TrackKind::Audio,
                keyframe: false,
                pts_us,
                data: data.freeze(),
            };
            broadcast.write(frame).unwrap();
            pts_us += 21_333;
            compio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

async fn input_bytes(engine: &MediaEngine) -> Option<(String, u64)> {
    let sessions = engine.ingests.sessions.read().await;
    sessions.get(INPUT_ID).map(|ingest| {
        (
            ingest.protocol.clone(),
            ingest.bytes_received.load(Ordering::Relaxed),
        )
    })
}

/// A listener on an ephemeral loopback port with the loopback certificate.
async fn listen(max_connections: usize, name: &str) -> (Arc<MediaEngine>, SocketAddr) {
    let dir = std::env::temp_dir().join(format!("restream-moq-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (cert, key) = &*LOOPBACK_CERT;
    let (cert_path, key_path) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert_path, cert).unwrap();
    std::fs::write(&key_path, key).unwrap();
    let config = MoqIngestConfig {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        tls_cert_path: cert_path.display().to_string(),
        tls_key_path: key_path.display().to_string(),
        max_connections,
        origin_max_viewers: 4,
    };
    let engine = Arc::new(MediaEngine::new());
    let server = Arc::new(MoqIngestServer::new(
        Arc::new(OneKey),
        engine.clone(),
        security(),
    ));
    let addr = start(&config, server).await.unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    (engine, addr)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connections_past_the_cap_are_refused() {
    let (engine, addr) = listen(1, "cap").await;
    tokio::task::spawn_blocking(move || {
        run(async move {
            let endpoint = transport::client_endpoint(AddressFamily::V4).await.unwrap();
            let tls = || loopback_client_tls(transport::CLIENT_ALPNS);
            // The first holds the only slot while the listener awaits its SETUP.
            let _first = transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls())
                .await
                .expect("the first connection is served");
            let second = transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls()).await;
            assert!(second.is_err(), "a connection past the cap is refused");
        });
    })
    .await
    .unwrap();
    engine.shutdown_listeners();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admitted_viewer_leaves_the_connection_cap_to_publishers() {
    let (engine, addr) = listen(2, "viewer-cap").await;
    let done = Arc::new(AtomicBool::new(false));
    let publisher = tokio::task::spawn_blocking({
        let done = done.clone();
        move || publish_until(addr, done)
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while input_bytes(&engine)
        .await
        .is_none_or(|(_, bytes)| bytes == 0)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the publisher never fed the input"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::task::spawn_blocking(move || {
        run(async move {
            let endpoint = transport::client_endpoint(AddressFamily::V4).await.unwrap();
            let tls = || loopback_client_tls(transport::CLIENT_ALPNS);
            // The publisher holds one slot; this viewer takes the other while
            // it handshakes, then watches the pipeline.
            let client = transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls())
                .await
                .expect("the viewer connects");
            let (token, _) = crate::media::moq::origin::mint_token("pipe-moq");
            let (viewer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(
                moq_net::Hop::random(),
            ));
            compio::runtime::spawn(transport::run_driver(driver)).detach();
            let (_session, driver) = moq_net::Client::new()
                .with_subscriber(viewer.clone())
                .with_path(format!("/?token={token}"))
                .connect_lite(Instant::now(), client)
                .await
                .unwrap();
            compio::runtime::spawn(transport::run_driver(driver)).detach();
            let consumer = viewer.consume();
            consumer.routed("pipe-moq").await.unwrap();
            let broadcast = consumer.request_broadcast("pipe-moq").await.unwrap();
            let mut catalog = broadcast
                .track("catalog.json")
                .unwrap()
                .subscribe(None)
                .await
                .unwrap();
            assert!(
                catalog.recv_group().await.unwrap().is_some(),
                "the viewer is served"
            );
            // Both slots were taken before the viewer was admitted; a
            // reconnecting publisher's handshake must still be accepted.
            transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls())
                .await
                .expect("a handshake is accepted while a viewer watches");
        });
    })
    .await
    .unwrap();
    done.store(true, Ordering::Release);
    publisher.await.unwrap();
    engine.shutdown_listeners();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_publisher_feeds_a_moq_input_and_shutdown_joins_the_thread() {
    let (engine, addr) = listen(4, "publish").await;

    let done = Arc::new(AtomicBool::new(false));
    let client = tokio::task::spawn_blocking({
        let done = done.clone();
        move || publish_until(addr, done)
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some((protocol, bytes)) = input_bytes(&engine).await
            && bytes > 0
        {
            assert_eq!(protocol, "moq");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no MoQ media reached the input"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    done.store(true, Ordering::Release);
    client.await.unwrap();

    engine.shutdown_listeners();
    let threads = engine.drain_os_thread_handles();
    assert_eq!(threads.len(), 1, "the listener registered its thread");
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            threads
                .into_iter()
                .for_each(|thread| thread.join().unwrap())
        }),
    )
    .await
    .expect("the listener thread stops on shutdown")
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while input_bytes(&engine).await.is_some() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the input stays registered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
