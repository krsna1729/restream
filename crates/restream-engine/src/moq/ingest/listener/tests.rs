//! The listener thread: absent unless configured; a real publisher routed
//! through it into a registered `moq` input; shutdown stops and joins it.

use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;

use super::*;
use crate::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessError, PipelineAccessFuture, PipelineAccessMode,
};
use crate::moq::egress::media::MediaFrame;
use crate::moq::egress::publish::HangBroadcast;
use crate::moq::ingest::session::INJECTED_PANIC_STREAM_KEY;
use crate::moq::transport::test_support::{
    LOOPBACK_CERT, LOOPBACK_SERVER_NAME, loopback_client_tls, run,
};
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_moq::media::AudioConfig;
use restream_moq::media::LocBuffer;
use restream_moq::media::TrackContainer;
use restream_moq::media::TrackKind;
use restream_moq::media::hang_catalog;
use restream_srt::address_family::AddressFamily;

const INPUT_ID: &str = "input-moq";

/// Accepts stream key `moq-key` for MoQ publish only, and the injected-panic
/// key for another pipeline's input.
struct OneKey;

impl PipelineAccessAuthenticator for OneKey {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            let (id, input_id) = match stream_key {
                "moq-key" => ("pipe-moq", INPUT_ID),
                INJECTED_PANIC_STREAM_KEY => ("pipe-doomed", "input-doomed"),
                _ => return Err(PipelineAccessError::InvalidStreamKey),
            };
            if mode != PipelineAccessMode::MoqPublish {
                return Err(PipelineAccessError::InvalidStreamKey);
            }
            Ok(AuthenticatedPipeline {
                id: id.to_string(),
                input_id: input_id.to_string(),
                selected: true,
            })
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
        assert!(MoqListener::new(&setting, Arc::new(OneKey), engine.clone(), security()).is_none());
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
        max_connections_per_ip: 4,
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
        let tls = loopback_client_tls(transport::MOQ_ALPNS);
        let session = transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls)
            .await
            .unwrap();
        let (origin, origin_driver, budget) = restream_moq::budget::origin();
        compio::runtime::spawn(transport::run_driver(origin_driver)).detach();
        let mut broadcast = HangBroadcast::create(&origin, budget, "moq-key", false, true).unwrap();
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
            .connect(Instant::now(), session)
            .await
            .unwrap();
        compio::runtime::spawn(transport::run_driver(driver)).detach();
        let mut pts_us = 0;
        while !done.load(Ordering::Acquire) {
            let frame = MediaFrame {
                track: TrackKind::Audio,
                keyframe: false,
                pts_us,
                data: LocBuffer::from_payload(&[0x21, 0x10, 0x05]),
            };
            broadcast.write(frame).unwrap();
            pts_us += 21_333;
            compio::time::sleep(Duration::from_millis(10)).await;
        }
    });
}

/// A catalog whose H.264 codec string is six bytes but five characters
/// (`Ψ` is two bytes): hang 0.21.16 sliced it at byte offsets and panicked
/// off a char boundary (found by the `moq_ingest` fuzz target).
const NON_ASCII_CODEC_CATALOG: &str = r#"{"video":{"renditions":{"video":{"codec":"avc1.4Ψ01e","codedWidth":64,"codedHeight":36,"container":{"kind":"legacy"}}}}}"#;

/// Publish a raw catalog until the listener closes the publisher session.
fn publish_raw_catalog(addr: SocketAddr, broadcast_name: &str, catalog_name: &str, catalog: Bytes) {
    run(async move {
        let endpoint = transport::client_endpoint(AddressFamily::V4).await.unwrap();
        let tls = loopback_client_tls(transport::MOQ_ALPNS);
        let session = transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls)
            .await
            .unwrap();
        let (origin, origin_driver) =
            moq_net::origin::Producer::new(moq_net::origin::Config::default());
        compio::runtime::spawn(transport::run_driver(origin_driver)).detach();
        let broadcast = origin.create_broadcast(broadcast_name).unwrap();
        let track = broadcast
            .create_track(catalog_name, hang::Catalog::default_track_info())
            .unwrap();
        let mut group = track.append_group().unwrap();
        group
            .write_frame(moq_net::Timestamp::now(), catalog)
            .unwrap();
        group.finish().unwrap();
        broadcast.announce(Default::default()).unwrap();
        let (session, driver) = moq_net::Client::new()
            .with_publisher(&origin)
            .connect(Instant::now(), session)
            .await
            .unwrap();
        compio::runtime::spawn(transport::run_driver(driver)).detach();
        compio::time::timeout(Duration::from_secs(10), session.closed())
            .await
            .expect("the listener closes the session");
    })
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

/// The loopback certificate's PEM files in a directory of their own,
/// removed on drop. A listener reads them at every start.
struct Certs(std::path::PathBuf);

impl Certs {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("restream-moq-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (cert, key) = &*LOOPBACK_CERT;
        std::fs::write(dir.join("cert.pem"), cert).unwrap();
        std::fs::write(dir.join("key.pem"), key).unwrap();
        Self(dir)
    }

    /// A loopback listener on an ephemeral port. Every test client is
    /// 127.0.0.1, so its share covers both caps unless a test narrows it.
    fn config(&self, max_connections: usize) -> MoqIngestConfig {
        MoqIngestConfig {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            tls_cert_path: self.0.join("cert.pem").display().to_string(),
            tls_key_path: self.0.join("key.pem").display().to_string(),
            max_connections,
            origin_max_viewers: 4,
            max_connections_per_ip: max_connections + 4,
        }
    }
}

impl Drop for Certs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn server(engine: &Arc<MediaEngine>) -> Arc<MoqIngestServer> {
    Arc::new(MoqIngestServer::new(
        Arc::new(OneKey),
        engine.clone(),
        security(),
    ))
}

/// A listener on an ephemeral loopback port with the loopback certificate.
async fn listen(max_connections: usize, name: &str) -> (Arc<MediaEngine>, SocketAddr) {
    let certs = Certs::new(name);
    let engine = Arc::new(MediaEngine::new());
    let (addr, _ended) = start(&certs.config(max_connections), server(&engine))
        .await
        .unwrap();
    (engine, addr)
}

/// One client (127.0.0.1) holds at most its share of the connections, and
/// another client (127.0.0.2) still connects while it does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_client_cannot_take_every_moq_connection_slot() {
    let certs = Certs::new("per-client");
    let config = MoqIngestConfig {
        max_connections_per_ip: 2,
        ..certs.config(4)
    };
    let engine = Arc::new(MediaEngine::new());
    let (addr, _ended) = start(&config, server(&engine)).await.unwrap();
    tokio::task::spawn_blocking(move || {
        run(async move {
            let tls = || loopback_client_tls(transport::MOQ_ALPNS);
            let greedy = Endpoint::client("127.0.0.1:0").await.unwrap();
            // Each holds a slot while the listener awaits its SETUP.
            let _first = transport::connect(&greedy, addr, LOOPBACK_SERVER_NAME, tls())
                .await
                .expect("the client's first connection is served");
            let _second = transport::connect(&greedy, addr, LOOPBACK_SERVER_NAME, tls())
                .await
                .expect("the client's second connection is served");
            let third = transport::connect(&greedy, addr, LOOPBACK_SERVER_NAME, tls()).await;
            assert!(
                third.is_err(),
                "a connection past the client's share is refused"
            );
            let other = Endpoint::client("127.0.0.2:0").await.unwrap();
            transport::connect(&other, addr, LOOPBACK_SERVER_NAME, tls())
                .await
                .expect("another client connects while the first holds its share");
        });
    })
    .await
    .unwrap();
    engine.shutdown_listeners();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connections_past_the_cap_are_refused() {
    let (engine, addr) = listen(1, "cap").await;
    tokio::task::spawn_blocking(move || {
        run(async move {
            let endpoint = transport::client_endpoint(AddressFamily::V4).await.unwrap();
            let tls = || loopback_client_tls(transport::MOQ_ALPNS);
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
            let tls = || loopback_client_tls(transport::MOQ_ALPNS);
            // The publisher holds one slot; this viewer takes the other while
            // it handshakes, then watches the pipeline.
            let client = transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls())
                .await
                .expect("the viewer connects");
            let (token, _) = crate::moq::origin::mint_token("pipe-moq");
            let (viewer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(
                moq_net::Hop::random(),
            ));
            compio::runtime::spawn(transport::run_driver(driver)).detach();
            let (_session, driver) = moq_net::Client::new()
                .with_subscriber(viewer.clone())
                .with_path(format!("/?token={token}"))
                .connect(Instant::now(), client)
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

/// Wait until a publisher on key `moq-key` feeds the input.
async fn wait_for_media(engine: &MediaEngine, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while input_bytes(engine)
        .await
        .is_none_or(|(_, bytes)| bytes == 0)
    {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Q-120: a faulted listener is restarted. The accept loop panics on its
/// first connection (`INJECTED_PANIC_MAX_CONNECTIONS`), outside every
/// connection task: the thread-entry boundary counts it, the thread ends and
/// `MoqListener::run` returns, which is what `SupervisedListener` waits on.
/// Running the listener again on the same port, as the supervisor does,
/// serves a publisher, and at shutdown its thread joins and its run returns.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_faulted_listener_returns_and_its_restart_on_the_same_port_serves() {
    let certs = Certs::new("restart");
    let engine = Arc::new(MediaEngine::new());
    let faulted = Arc::new(MoqListener {
        config: certs.config(INJECTED_PANIC_MAX_CONNECTIONS),
        server: server(&engine),
    });
    let (started, addr) = tokio::sync::oneshot::channel();
    let faulted = tokio::spawn(faulted.run(Some(started)));
    let addr = addr.await.expect("the listener starts");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !faulted.is_finished(),
        "a healthy listener's run keeps waiting on its thread"
    );
    let before = crate::panic_boundary::contained_panics();
    tokio::task::spawn_blocking(move || {
        run(async move {
            let endpoint = transport::client_endpoint(AddressFamily::V4).await.unwrap();
            let tls = loopback_client_tls(transport::MOQ_ALPNS);
            // The listener panics on this connection; the attempt only has
            // to reach it.
            let _ = compio::time::timeout(
                Duration::from_secs(1),
                transport::connect(&endpoint, addr, LOOPBACK_SERVER_NAME, tls),
            )
            .await;
        });
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), faulted)
        .await
        .expect("a faulted listener's run returns, so its supervisor restarts it")
        .expect("no panic escapes the listener thread");
    assert!(
        crate::panic_boundary::contained_panics() > before,
        "the accept-loop panic is counted at the thread entry"
    );

    // What `SupervisedListener` does: run the listener again, on the port
    // the faulted one held (without the injected cap).
    let restarted = Arc::new(MoqListener {
        config: MoqIngestConfig {
            listen: addr,
            ..certs.config(4)
        },
        server: server(&engine),
    });
    let (started, restarted_addr) = tokio::sync::oneshot::channel();
    let restarted = tokio::spawn(restarted.run(Some(started)));
    assert_eq!(restarted_addr.await.expect("the restart binds"), addr);
    let done = Arc::new(AtomicBool::new(false));
    let publisher = tokio::task::spawn_blocking({
        let done = done.clone();
        move || publish_until(addr, done)
    });
    wait_for_media(
        &engine,
        "a publisher on the restarted listener never fed the input",
    )
    .await;
    done.store(true, Ordering::Release);
    publisher.await.unwrap();

    engine.shutdown_listeners();
    let threads = engine.drain_os_thread_handles();
    assert!(
        !threads.is_empty(),
        "the restarted listener registered its thread"
    );
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            threads
                .into_iter()
                .for_each(|thread| thread.join().expect("no panic escapes a listener thread"))
        }),
    )
    .await
    .expect("every listener thread joins at shutdown")
    .unwrap();
    tokio::time::timeout(Duration::from_secs(1), restarted)
        .await
        .expect("the restarted listener's run returns once its thread joined")
        .unwrap();
}

/// A panic while serving one connection (injected in a publisher session
/// right after its admission) ends only that connection: counted, its input
/// attempt released, and a publisher already feeding another input on the
/// same listener thread keeps feeding it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_panicking_connection_ends_alone_and_the_listener_keeps_serving() {
    let (engine, addr) = listen(4, "connection-panic").await;
    let done = Arc::new(AtomicBool::new(false));
    let healthy = tokio::task::spawn_blocking({
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
            "the healthy publisher never fed its input"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let before = crate::panic_boundary::contained_panics();
    tokio::task::spawn_blocking(move || {
        publish_raw_catalog(
            addr,
            INJECTED_PANIC_STREAM_KEY,
            hang::Catalog::DEFAULT_NAME,
            Bytes::from_static(b"{}"),
        )
    })
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while crate::panic_boundary::contained_panics() == before
        || engine
            .input_has_session("pipe-doomed", "input-doomed")
            .await
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the panic was not counted at the connection boundary, or its input stayed registered"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let (_, fed) = input_bytes(&engine).await.expect("the healthy input stays");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while input_bytes(&engine)
        .await
        .is_none_or(|(_, bytes)| bytes <= fed)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the healthy publisher stopped feeding after the panic"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    done.store(true, Ordering::Release);
    healthy.await.unwrap();
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

/// A catalog hang refuses ends that publisher's session with its input
/// released and the reason recorded, and the listener serves the next
/// publisher. If hang panicked on this catalog, Compio's task boundary would
/// drop the session and the input would be released as abandoned.
async fn assert_catalog_rejection_releases_only_its_publisher(
    name: &str,
    broadcast_name: &'static str,
    catalog_name: &'static str,
    catalog: Bytes,
    expected_reason: &str,
) {
    let (engine, addr) = listen(4, name).await;
    tokio::task::spawn_blocking(move || {
        publish_raw_catalog(addr, broadcast_name, catalog_name, catalog)
    })
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let reason = loop {
        if input_bytes(&engine).await.is_none()
            && let Some(outcome) = engine.recent_ingest_outcome("pipe-moq").await
        {
            break outcome.reason.unwrap_or_default();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the input is never released"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(reason, expected_reason);

    // The listener thread still serves the next publisher.
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
            "the next publisher never fed the input"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    done.store(true, Ordering::Release);
    publisher.await.unwrap();
    engine.shutdown_listeners();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_catalog_hang_cannot_parse_ends_only_that_publisher() {
    assert_catalog_rejection_releases_only_its_publisher(
        "bad-catalog",
        "moq-key",
        hang::Catalog::DEFAULT_NAME,
        Bytes::from_static(NON_ASCII_CODEC_CATALOG.as_bytes()),
        "catalog.json/MSF catalog: hang: json error: json: video.renditions.video.codec: invalid codec",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_oversized_shared_init_msf_catalog_ends_only_that_publisher() {
    let init_size = moq_msf::MAX_CATALOG_BYTES / moq_msf::MAX_TRACKS + 1;
    let tracks: Vec<_> = (0..moq_msf::MAX_TRACKS)
        .map(|index| {
            serde_json::json!({
                "name": format!("audio-{index}"),
                "packaging": "loc",
                "role": "audio",
                "codec": "mp4a.40.2",
                "initRef": "shared",
            })
        })
        .collect();
    let catalog = serde_json::to_vec(&serde_json::json!({
        "version": "draft-01",
        "initDataList": [{"id": "shared", "type": "inline", "data": "X".repeat(init_size)}],
        "tracks": tracks,
    }))
    .unwrap();
    assert!(catalog.len() < moq_msf::MAX_CATALOG_BYTES);
    assert!(init_size * moq_msf::MAX_TRACKS > moq_msf::MAX_CATALOG_BYTES);
    // The transport sees only ~0.5 MiB. The upstream parser must reject the
    // >32 MiB resolved total before cloning initRef payloads or converting
    // AAC: without that guard this would fail later as malformed init data.
    assert_catalog_rejection_releases_only_its_publisher(
        "oversized-shared-init",
        "moq-key.msf",
        moq_msf::DEFAULT_NAME,
        Bytes::from(catalog),
        "catalog.json/MSF catalog: msf: failed to parse MSF catalog frame",
    )
    .await;
}
