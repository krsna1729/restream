//! Shared fixtures for the SRT ingress live tests: a real listener server, a real
//! srt-rs caller on its own thread, and the checked MPEG-TS fixture.

use std::collections::HashMap;
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
use super::tokio_ingress::{SrtIngestPolicyStore, SrtServer};

pub(super) const CALLER_TX: usize = 16;
pub(super) const CHUNK: usize = 1316;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The first `chunks` SRT-message-sized slices of the canonical H.264 TS.
pub(super) fn ts_chunks(chunks: usize) -> Vec<Bytes> {
    let path = crate::test_fixtures::canonical_h264_ts_fixture().expect("checked-in fixture");
    let bytes = std::fs::read(path).expect("read fixture");
    bytes
        .chunks_exact(CHUNK)
        .take(chunks)
        .map(Bytes::copy_from_slice)
        .collect()
}

/// Accepts every stream key the test names; anything else is unauthorized.
pub(super) struct TestAuth {
    pub(super) pipelines: HashMap<String, AuthenticatedPipeline>,
}

impl TestAuth {
    pub(super) fn new(keys: &[&str]) -> Arc<Self> {
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

pub(super) fn policy_entry(key: &str, policy: SrtPipelineIngestConfig) -> SrtIngestPolicyEntry {
    SrtIngestPolicyEntry::new(format!("pipeline-{key}"), key, policy)
}

pub(super) fn plain(key: &str) -> SrtIngestPolicyEntry {
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

pub(super) fn encrypted(key: &str, passphrase: &str, pbkeylen: i32) -> SrtIngestPolicyEntry {
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
pub(super) fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

pub(super) struct TestServer {
    pub(super) server: Arc<SrtServer>,
    pub(super) engine: Arc<MediaEngine>,
    pub(super) remote: SocketAddr,
    pub(super) task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    pub(super) async fn start(auth_keys: &[&str], entries: Vec<SrtIngestPolicyEntry>) -> Self {
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

    pub(super) async fn stop(self) {
        self.engine.shutdown_listeners();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("the listener stops")
            .expect("the listener task did not panic");
    }

    pub(super) async fn ring_progress(&self, pipeline_id: &str) -> usize {
        self.engine
            .get_or_create_pipeline(pipeline_id)
            .await
            .get_write_idx()
    }

    pub(super) async fn ingest_active(&self, pipeline_id: &str) -> bool {
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
    pub(super) async fn diagnostics(&self) -> String {
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

    pub(super) async fn wait_until<F, Fut>(&self, what: &str, timeout: Duration, mut condition: F)
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
pub(super) struct CallerReport {
    pub(super) connected: bool,
    pub(super) disconnected: bool,
    pub(super) connected_at: Option<Instant>,
    pub(super) disconnected_at: Option<Instant>,
    pub(super) sent_bytes: usize,
    pub(super) received: Vec<u8>,
    pub(super) group_faults: usize,
    pub(super) max_active_legs: usize,
}

pub(super) enum Attach {
    Direct(Box<CallerConfig>),
    Bonded(BondedCallerConfig),
}

pub(super) struct CallerCtx<'a> {
    pub(super) owner: &'a mut Owner,
    pub(super) id: srt_transport::advanced::caller::LogicalCallerId,
    pub(super) now: Timestamp,
    pub(super) report: &'a mut CallerReport,
}

impl CallerCtx<'_> {
    pub(super) fn connected(&mut self) -> bool {
        self.owner
            .logical_caller(&self.id)
            .and_then(|caller| caller.state())
            == Some(LogicalCallerState::Connected)
    }

    /// Offer one payload; `false` when the send window is closed.
    pub(super) fn send(&mut self, payload: &Bytes) -> bool {
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

pub(super) fn session(stream_id: &str, crypto: Option<(&str, KeyLength)>) -> SessionConfig {
    let mut session = SessionConfig::default();
    session.set_stream_id(Some(stream_id.to_string()));
    if let Some((passphrase, key_length)) = crypto {
        session.set_encryption(Some(
            EncryptionConfig::new(passphrase).key_length(key_length),
        ));
    }
    session
}

pub(super) fn direct(
    remote: SocketAddr,
    stream_id: &str,
    crypto: Option<(&str, KeyLength)>,
) -> Attach {
    Attach::Direct(Box::new(leg(remote, stream_id, crypto)))
}

pub(super) fn leg(
    remote: SocketAddr,
    stream_id: &str,
    crypto: Option<(&str, KeyLength)>,
) -> CallerConfig {
    CallerConfig::builder(remote)
        .ownership(SocketOwnership::Shared)
        .session(session(stream_id, crypto))
        .build()
        .expect("caller config")
}

pub(super) fn bonded(remote: SocketAddr, stream_id: &str, mode: GroupType) -> Attach {
    Attach::Bonded(
        BondedCallerConfig::new(GroupConfig::new(0x0BAD_CA11, mode))
            .leg(leg(remote, stream_id, None), 20)
            .leg(leg(remote, stream_id, None), 10),
    )
}

/// Run a caller until `step` returns `true` or `deadline` passes. Blocking:
/// call from `spawn_blocking`.
pub(super) fn run_caller(
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
pub(super) fn publish_step(chunks: Vec<Bytes>) -> impl FnMut(&mut CallerCtx<'_>) -> bool {
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

pub(super) async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work)
        .await
        .expect("caller thread")
}
