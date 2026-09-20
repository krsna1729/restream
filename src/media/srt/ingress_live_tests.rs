//! Live tests of the SRT ingress owner: a real `srt_transport::compio::Owner`
//! listener on a real UDP socket, driven by a real srt-rs caller (direct and
//! bonded) on its own thread, feeding the real Restream session/media path.
//!
//! These prove the properties only a real Owner session can: asynchronous
//! rejection and pipeline deletion actually disconnect the protocol peer, SRT
//! read/play sends through the Owner's logical peer, bonded Broadcast/Backup
//! ingest reaches Tokio as one logical peer without duplicated application
//! bytes, and the thread bridges never silently lose accepted media.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use bytes::Bytes;
use srt_proto::Timestamp;
use srt_proto::crypto::KeyLength;
use srt_proto::handshake::GroupType;
use srt_transport::advanced::caller::{LogicalCallerState, LogicalCallerStats, PoolOutcome};
use srt_transport::compio::{Owner, OwnerServiceBudget, ProductionRuntimeConfig};
use srt_transport::{
    BondedCallerConfig, CallerConfig, EncryptionConfig, GroupConfig, SessionConfig, SocketOwnership,
};

use crate::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
use crate::domain::srt_ingest::{
    SrtGlobalIngestConfig, SrtPipelineIngestConfig, SrtPipelineIngestMode,
};
use crate::media::egress::backends::srt::owner_set::{SRT_OWNER_WIRE_CEILING, production_runtime};
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessFuture,
    PipelineAccessMode,
};
use crate::media::security::IngestSecurityService;

use super::SrtIngestPolicyEntry;
use super::ingress_admission::ReceiverGroupId;
use super::ingress_owner::{IngressConfig, SrtIngressEvent, SrtIngressHandle};
use super::tokio_ingress::{SrtIngestPolicyStore, SrtServer};

const CALLER_TX: usize = 16;
const CHUNK: usize = 1316;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The first `chunks` SRT-message-sized slices of the canonical H.264 TS.
fn ts_chunks(chunks: usize) -> Vec<Bytes> {
    let path = crate::test_fixtures::canonical_h264_ts_fixture().expect("checked-in fixture");
    let bytes = std::fs::read(path).expect("read fixture");
    bytes
        .chunks_exact(CHUNK)
        .take(chunks)
        .map(Bytes::copy_from_slice)
        .collect()
}

/// Accepts every stream key the test names; anything else is unauthorized.
struct TestAuth {
    pipelines: HashMap<String, AuthenticatedPipeline>,
}

impl TestAuth {
    fn new(keys: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            pipelines: keys
                .iter()
                .map(|key| {
                    (
                        (*key).to_string(),
                        AuthenticatedPipeline {
                            id: format!("pipeline-{key}"),
                            input_id: format!("input-{key}"),
                            selected: true,
                        },
                    )
                })
                .collect(),
        })
    }
}

impl PipelineAccessAuthenticator for TestAuth {
    fn authenticate<'a>(
        &'a self,
        _mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            self.pipelines
                .get(stream_key)
                .cloned()
                .ok_or(PipelineAccessError::InvalidStreamKey)
        })
    }
}

fn policy_entry(key: &str, policy: SrtPipelineIngestConfig) -> SrtIngestPolicyEntry {
    SrtIngestPolicyEntry::new(format!("pipeline-{key}"), key, policy)
}

fn plain(key: &str) -> SrtIngestPolicyEntry {
    policy_entry(
        key,
        SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Plaintext,
            passphrase: None,
            pbkeylen: None,
            latency_ms: Some(60),
        },
    )
}

fn encrypted(key: &str, passphrase: &str, pbkeylen: i32) -> SrtIngestPolicyEntry {
    policy_entry(
        key,
        SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Encrypted,
            passphrase: Some(passphrase.to_string()),
            pbkeylen: Some(pbkeylen),
            latency_ms: Some(60),
        },
    )
}

/// A free UDP port (bound and released; nothing else in the test binds it).
fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

struct TestServer {
    server: Arc<SrtServer>,
    engine: Arc<MediaEngine>,
    remote: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    async fn start(auth_keys: &[&str], entries: Vec<SrtIngestPolicyEntry>) -> Self {
        let engine = Arc::new(MediaEngine::new());
        let store = Arc::new(SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &entries,
        ));
        let server = Arc::new(SrtServer::new(
            TestAuth::new(auth_keys),
            engine.clone(),
            Arc::new(IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG)),
            store,
        ));
        let port = free_port();
        let task = tokio::spawn(server.clone().run(port));
        let ready = engine.listener_stats_handle();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.bonding_available.load(Ordering::Relaxed) {
            assert!(
                Instant::now() < deadline,
                "the SRT listener did not become ready"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Self {
            server,
            engine,
            remote: SocketAddr::from(([127, 0, 0, 1], port)),
            task,
        }
    }

    async fn stop(self) {
        self.engine.shutdown_listeners();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("the listener stops")
            .expect("the listener task did not panic");
    }

    async fn ring_progress(&self, pipeline_id: &str) -> usize {
        self.engine
            .get_or_create_pipeline(pipeline_id)
            .await
            .get_write_idx()
    }

    async fn ingest_active(&self, pipeline_id: &str) -> bool {
        self.engine
            .ingests
            .active
            .read()
            .await
            .contains_key(pipeline_id)
    }

    /// Failure-only state dump: enough to tell a handshake failure, an
    /// authorization failure, an Owner fault, bridge starvation, a vanished
    /// session and plain host scheduling delay apart. Never called on the
    /// success path and never per packet.
    async fn diagnostics(&self) -> String {
        let snapshot = self.engine.listener_stats_handle().ingress_owner.snapshot();
        let pipelines: Vec<String> = self
            .engine
            .ingests
            .pipelines
            .read()
            .await
            .keys()
            .cloned()
            .collect();
        let active: Vec<String> = self
            .engine
            .ingests
            .active
            .read()
            .await
            .keys()
            .cloned()
            .collect();
        format!(
            "srt_server_task_alive={} pipelines={pipelines:?} active_ingests={active:?} \
             ingressOwner{{faulted={} managedRx={} peers={} serviceVisits={} serviceActions={} txPackets={} \
             txCompletedOk={} txInFlight={} rxPackets={} rxRingDropped={} rxTruncated={} \
             eventDepthHwm={} commandDepthHwm={} eventBridgeFullVisits={} staleCommands={} \
             overloadDisconnects={} policyRequests={} policyRejections={} credentialFailures={}}}",
            !self.task.is_finished(),
            snapshot.faulted,
            snapshot.managed_rx,
            snapshot.peers,
            snapshot.service_visits,
            snapshot.service_actions,
            snapshot.tx_packets,
            snapshot.tx_completed_ok,
            snapshot.tx_in_flight,
            snapshot.rx_packets,
            snapshot.rx_ring_dropped,
            snapshot.rx_truncated,
            snapshot.event_depth_hwm,
            snapshot.command_depth_hwm,
            snapshot.event_bridge_full_visits,
            snapshot.stale_commands,
            snapshot.overload_disconnects,
            snapshot.policy_requests,
            snapshot.policy_rejections,
            snapshot.credential_failures,
        )
    }

    async fn wait_until<F, Fut>(&self, what: &str, timeout: Duration, mut condition: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = Instant::now() + timeout;
        while !condition().await {
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for: {what}; {}",
                    self.diagnostics().await
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// A real srt-rs caller on its own thread (production Compio runtime + Owner)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct CallerReport {
    connected: bool,
    disconnected: bool,
    connected_at: Option<Instant>,
    disconnected_at: Option<Instant>,
    sent_bytes: usize,
    received: Vec<u8>,
    group_faults: usize,
    max_active_legs: usize,
}

enum Attach {
    Direct(Box<CallerConfig>),
    Bonded(BondedCallerConfig),
}

struct CallerCtx<'a> {
    owner: &'a mut Owner,
    id: srt_transport::advanced::caller::LogicalCallerId,
    now: Timestamp,
    report: &'a mut CallerReport,
}

impl CallerCtx<'_> {
    fn connected(&mut self) -> bool {
        self.owner
            .logical_caller(&self.id)
            .and_then(|caller| caller.state())
            == Some(LogicalCallerState::Connected)
    }

    /// Offer one payload; `false` when the send window is closed.
    fn send(&mut self, payload: &Bytes) -> bool {
        let Some(mut caller) = self.owner.logical_caller_mut(&self.id) else {
            return false;
        };
        if !caller.can_send() {
            return false;
        }
        match caller.send_shared(payload.clone(), self.now) {
            Ok(legs) if legs > 0 => {
                self.report.sent_bytes += payload.len();
                true
            }
            _ => false,
        }
    }
}

fn session(stream_id: &str, crypto: Option<(&str, KeyLength)>) -> SessionConfig {
    let mut session = SessionConfig::default();
    session.set_stream_id(Some(stream_id.to_string()));
    if let Some((passphrase, key_length)) = crypto {
        session.set_encryption(Some(
            EncryptionConfig::new(passphrase).key_length(key_length),
        ));
    }
    session
}

fn direct(remote: SocketAddr, stream_id: &str, crypto: Option<(&str, KeyLength)>) -> Attach {
    Attach::Direct(Box::new(leg(remote, stream_id, crypto)))
}

fn leg(remote: SocketAddr, stream_id: &str, crypto: Option<(&str, KeyLength)>) -> CallerConfig {
    CallerConfig::builder(remote)
        .ownership(SocketOwnership::Shared)
        .session(session(stream_id, crypto))
        .build()
        .expect("caller config")
}

fn bonded(remote: SocketAddr, stream_id: &str, mode: GroupType) -> Attach {
    Attach::Bonded(
        BondedCallerConfig::new(GroupConfig::new(0x0BAD_CA11, mode))
            .leg(leg(remote, stream_id, None), 20)
            .leg(leg(remote, stream_id, None), 10),
    )
}

/// Run a caller until `step` returns `true` or `deadline` passes. Blocking:
/// call from `spawn_blocking`.
fn run_caller(
    attach: Attach,
    deadline: Duration,
    mut step: impl FnMut(&mut CallerCtx<'_>) -> bool,
) -> CallerReport {
    let runtime = production_runtime(ProductionRuntimeConfig::for_owner(
        CALLER_TX,
        SRT_OWNER_WIRE_CEILING,
    ))
    .expect("io_uring runtime for the test caller");
    runtime.block_on(async {
        let mut report = CallerReport::default();
        let mut owner = Owner::new_with_ceiling(CALLER_TX, SRT_OWNER_WIRE_CEILING);
        let epoch = Instant::now();
        let timestamp = |epoch: Instant| Timestamp::from_micros(epoch.elapsed().as_micros() as u64);
        let outcome = match &attach {
            Attach::Direct(config) => owner.connect(config, timestamp(epoch)),
            Attach::Bonded(config) => owner.connect_bonded(config, timestamp(epoch)),
        }
        .expect("caller attaches");
        let PoolOutcome::Admitted(id) = outcome else {
            panic!("the caller was not admitted immediately: {outcome:?}");
        };
        let mut events = Vec::new();
        let mut done = false;
        while epoch.elapsed() < deadline && !done {
            let now = timestamp(epoch);
            let _ = owner.service(now, OwnerServiceBudget::default()).await;
            owner.poll_caller_events(&mut events);
            for event in events.drain(..) {
                match event.event {
                    srt_proto::ConnectionEvent::Connected => {
                        report.connected = true;
                        report.connected_at.get_or_insert_with(Instant::now);
                    }
                    srt_proto::ConnectionEvent::Disconnected { .. } => {
                        report.disconnected = true;
                        report.disconnected_at.get_or_insert_with(Instant::now);
                    }
                    srt_proto::ConnectionEvent::DataReceived { payload, .. } => {
                        report.received.extend_from_slice(&payload);
                    }
                    _ => {}
                }
            }
            let mut faults = Vec::new();
            owner.poll_caller_group_faults(8, &mut faults);
            report.group_faults += faults.len();
            if let Some(LogicalCallerStats::Group(stats)) =
                owner.logical_caller(&id).and_then(|caller| caller.stats())
            {
                report.max_active_legs = report.max_active_legs.max(stats.aggregate.active_legs);
            }
            let state = owner.logical_caller(&id).and_then(|caller| caller.state());
            if state == Some(LogicalCallerState::Connected) {
                // A bonded group reports connection through its state, not a
                // per-connection event.
                report.connected = true;
                report.connected_at.get_or_insert_with(Instant::now);
            }
            if state == Some(LogicalCallerState::Disconnected) {
                report.disconnected = true;
                report.disconnected_at.get_or_insert_with(Instant::now);
            }
            let mut ctx = CallerCtx {
                owner: &mut owner,
                id,
                now,
                report: &mut report,
            };
            done = step(&mut ctx);
            owner.wait_for_activity(Duration::from_millis(2)).await;
        }
        if let Some(mut caller) = owner.logical_caller_mut(&id) {
            caller.disconnect(timestamp(epoch));
        }
        for _ in 0..20 {
            let _ = owner
                .service(timestamp(epoch), OwnerServiceBudget::default())
                .await;
            owner.wait_for_activity(Duration::from_millis(2)).await;
        }
        let _ = owner.shutdown_and_drain(Duration::from_secs(3)).await;
        report
    })
}

/// Publish `chunks` over the caller as fast as the window allows, then linger
/// briefly so the receiver's latency window releases the tail.
fn publish_step(chunks: Vec<Bytes>) -> impl FnMut(&mut CallerCtx<'_>) -> bool {
    let mut next = 0;
    let mut sent_all_at: Option<Instant> = None;
    move |ctx| {
        if !ctx.connected() {
            return false;
        }
        while next < chunks.len() && ctx.send(&chunks[next]) {
            next += 1;
        }
        if next == chunks.len() {
            return sent_all_at.get_or_insert_with(Instant::now).elapsed()
                >= Duration::from_millis(600);
        }
        false
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .expect("caller thread")
}

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
