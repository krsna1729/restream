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

/// A port that was free when probed. The probe socket is released, so a
/// parallel test can take the port before the caller binds; `TestServer::start`
/// retries on a fresh port for exactly that reason.
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
        Self::start_with_engine(auth_keys, entries, Arc::new(MediaEngine::new())).await
    }

    pub(super) async fn start_with_engine(
        auth_keys: &[&str],
        entries: Vec<SrtIngestPolicyEntry>,
        engine: Arc<MediaEngine>,
    ) -> Self {
        // `free_port` releases its probe socket before the server binds, so a
        // parallel test can take the port in between. A listener that exits
        // early is retried on a fresh port; `SrtServer::run` logs its own start
        // error, so the panic below only reports that the attempts ran out.
        let mut last_error = None;
        for _ in 0..3 {
            let engine = engine.clone();
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
                if task.is_finished() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "the SRT listener did not become ready"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            if ready.bonding_available.load(Ordering::Relaxed) {
                return Self {
                    server,
                    engine,
                    remote: SocketAddr::from(([127, 0, 0, 1], port)),
                    task,
                };
            }
            last_error = Some(match task.await {
                Ok(()) => "the listener stopped before it was ready".to_string(),
                Err(error) => error.to_string(),
            });
        }
        panic!(
            "the SRT listener did not become ready on three ports; see the logged start error ({})",
            last_error.unwrap_or_default()
        );
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
             txCompletedOk={} txInFlight={} rxPackets={} rxRingDropped={} rxBufferExhaustions={} rxTruncated={} \
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
            snapshot.rx_buffer_exhaustions,
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

fn timestamp(epoch: Instant) -> Timestamp {
    Timestamp::from_micros(epoch.elapsed().as_micros() as u64)
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
                        report.received.reserve(payload.len());
                        for chunk in payload.chunks() {
                            report.received.extend_from_slice(chunk);
                        }
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

/// The public protocol message API can send a fragmented message; the logical
/// native caller's packet API deliberately does not flatten or split packets.
/// Drive that protocol over a real UDP socket into the production ingress Owner.
pub(super) fn run_fragmented_publisher(
    remote: SocketAddr,
    stream_id: &str,
    payload: &[u8],
    deadline: Duration,
) -> CallerReport {
    use srt_proto::{
        ConnectionEvent, ConnectionOptions, ConnectionOutput, ConnectionState, SrtConnection,
    };
    use srt_transport::advanced::driver::ManualTimerStore;

    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("publisher socket");
    socket
        .set_read_timeout(Some(Duration::from_millis(1)))
        .expect("receive timeout");
    let mut caller = SrtConnection::new_caller(ConnectionOptions {
        stream_id: Some(stream_id.to_owned()),
        // All fragments share one source timestamp. Allow their socket transfer
        // to finish before Live-mode late-drop can retire the message.
        tsbpd_delay: 2000,
        ..Default::default()
    });
    let mut timers = ManualTimerStore::new();
    let epoch = Instant::now();
    caller
        .connect(Timestamp::default())
        .expect("start publisher");
    let mut report = CallerReport::default();
    let mut buffer = [0u8; 1500];
    while epoch.elapsed() < deadline && !report.disconnected {
        let now = timestamp(epoch);
        timers.fire_expired(now, &mut caller);
        if caller.state() == ConnectionState::Connected && report.sent_bytes == 0 {
            caller
                .send_message(payload, now)
                .expect("whole fragmented message fits the negotiated window");
            report.sent_bytes = payload.len();
        }
        for _ in 0..64 {
            let Some(output) = caller.poll_output().expect("protocol output") else {
                break;
            };
            if let ConnectionOutput::SendPacket(packet) = &output {
                assert_eq!(
                    socket.send_to(packet, remote).expect("send datagram"),
                    packet.len()
                );
            }
            timers.apply_output(&output, now);
        }
        for _ in 0..64 {
            match socket.recv_from(&mut buffer) {
                Ok((len, peer)) => {
                    assert_eq!(peer, remote);
                    caller
                        .feed_recv_buf(&buffer[..len], timestamp(epoch))
                        .expect("listener datagram");
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => panic!("receive listener datagram: {error}"),
            }
        }
        while let Some(event) = caller.poll_event() {
            match event {
                ConnectionEvent::Connected => report.connected = true,
                ConnectionEvent::Disconnected { .. } => report.disconnected = true,
                _ => {}
            }
        }
    }
    if !report.disconnected {
        eprintln!(
            "fragmented publisher state={:?}, sender={:?}",
            caller.state(),
            caller.sender_stats()
        );
    }
    report
}

/// A single real SRT connection, controlled by the origin composition test.
/// The channel is only a fixture driver; production payloads still traverse
/// the native caller, UDP, ingress Owner and Tokio probe application.
pub(crate) struct TopologyPublisher {
    pub(crate) engine: Arc<MediaEngine>,
    batches: flume::Sender<Vec<Bytes>>,
    caller: tokio::task::JoinHandle<CallerReport>,
    server: TestServer,
}

impl TopologyPublisher {
    pub(crate) async fn start() -> Self {
        let server = TestServer::start(&["topology"], vec![plain("topology")]).await;
        let remote = server.remote;
        let (batches, receive) = flume::unbounded::<Vec<Bytes>>();
        let caller = tokio::task::spawn_blocking(move || {
            let mut pending = std::collections::VecDeque::new();
            run_caller(
                direct(remote, "#!::r=topology,m=publish", None),
                Duration::from_secs(30),
                move |ctx| {
                    if let Ok(batch) = receive.try_recv() {
                        pending.extend(batch);
                    }
                    while ctx.connected() && pending.front().is_some_and(|chunk| ctx.send(chunk)) {
                        pending.pop_front();
                    }
                    receive.is_disconnected() && pending.is_empty()
                },
            )
        });
        Self {
            engine: server.engine.clone(),
            batches,
            caller,
            server,
        }
    }

    pub(crate) fn send(&self, video: bool, audio: bool, version: u8) {
        self.batches
            .send(topology_chunks(video, audio, version))
            .expect("the SRT fixture caller is alive");
    }

    pub(crate) async fn stop(self) {
        drop(self.batches);
        let report = self.caller.await.expect("SRT caller thread");
        self.server.stop().await;
        assert!(report.connected && !report.disconnected, "{report:?}");
    }
}

/// Mutate only the checked-in fixture's PMTs and discard the removed PID.
/// All surviving elementary bytes, timestamps and decoder config are real
/// fixture bytes; no media parser or probe application is emulated here.
pub(super) fn topology_chunks(video: bool, audio: bool, version: u8) -> Vec<Bytes> {
    let fixture = ts_chunks(600);
    let mut probe = crate::media::mpegts::TsDemuxer::new();
    for chunk in &fixture {
        probe.feed(chunk);
    }
    probe.flush();
    let probe = probe.take_probe().expect("fixture A/V probe");
    let video_pid = probe.video.unwrap().pid.unwrap();
    let audio_pid = probe.audio_tracks[0].pid.unwrap();
    let pmt = topology_pmt(
        video.then_some(video_pid),
        if audio {
            std::slice::from_ref(&audio_pid)
        } else {
            &[]
        },
        version,
    );
    let mut output = Vec::new();
    for packet in fixture.iter().flat_map(|chunk| chunk.chunks_exact(188)) {
        let pid = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
        if (pid == video_pid && !video) || (pid == audio_pid && !audio) {
            continue;
        }
        if pid == 0x1000 {
            assert_eq!(&packet[..5], &[0x47, 0x50, 0, packet[3], 0]);
            let mut pmt = pmt.to_vec();
            pmt[3] = packet[3];
            output.extend_from_slice(&pmt);
        } else {
            output.extend_from_slice(packet);
        }
    }
    output.chunks(CHUNK).map(Bytes::copy_from_slice).collect()
}

pub(super) fn topology_pmt(video_pid: Option<u16>, audio_pids: &[u16], version: u8) -> Bytes {
    topology_pmt_typed(video_pid.map(|pid| (0x1B, pid)), audio_pids, version)
}

pub(super) fn topology_pmt_typed(
    video: Option<(u8, u16)>,
    audio_pids: &[u16],
    version: u8,
) -> Bytes {
    let pcr_pid = video
        .map(|(_, pid)| pid)
        .or_else(|| audio_pids.first().copied())
        .unwrap_or(0x1FFF);
    let mut section = vec![
        0x02,
        0xB0,
        0,
        0,
        1,
        0xC1 | ((version & 31) << 1),
        0,
        0,
        0xE0 | (pcr_pid >> 8) as u8,
        pcr_pid as u8,
        0xF0,
        0,
    ];
    for (kind, pid) in video
        .into_iter()
        .chain(audio_pids.iter().map(|pid| (0x0F, *pid)))
    {
        section.extend_from_slice(&[kind, 0xE0 | (pid >> 8) as u8, pid as u8, 0xF0, 0]);
    }
    section[2] = (section.len() + 1) as u8;
    let crc = crate::media::mpegts::test_crc32_mpeg2(&section);
    section.extend_from_slice(&crc.to_be_bytes());
    let mut pmt = [0xFF; 188];
    pmt[..5].copy_from_slice(&[0x47, 0x50, 0, 0x10, 0]);
    pmt[5..5 + section.len()].copy_from_slice(&section);
    Bytes::copy_from_slice(&pmt)
}
