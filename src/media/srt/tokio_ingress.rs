//! Tokio listener, admission, and media-session lifecycle for srt-rs ingress.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use shiguredo_srt::{ConnectionEvent, Timestamp};
use srt_transport::{
    AdmissionEvent, AdmissionResolution, BondedInputPolicy, HighResWaiter, IngressTelemetry,
    ListenerConfig, ListenerEncryptionConfig, ListenerPeerPolicy, ListenerTopology, LogicalPeerId,
    MonotonicDeadline, PeerTable, PolicyOverride, RecvBatch, RecvBudget, RejectionReason,
    RuntimeFlavor,
};
use tokio::net::UdpSocket;
use tracing::{error, info, warn};

use crate::domain::srt_ingest::ResolvedSrtCrypto;
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{PipelineAccessAuthenticator, PipelineAccessMode};
use crate::media::input_gate::InputTimestampMapper;
use crate::media::ring_buffer::RingBuffer;
use crate::media::security::{IngestSecurityService, RateLimitScope};
use crate::media::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};

use super::tokio_egress::timestamp_now;
use crate::media::standby_gop::StandbyGopCache;
use crate::media::ts_chunk_ring::TsChunkReader;

pub(crate) use super::srt_policy::SrtIngestPolicyStore;

#[path = "ingest_packets.rs"]
mod ingest_packets;

const SRT_MESSAGE_PAYLOAD_MAX: usize = 1316;
/// Upper bound for listener parks so reader pull and shutdown stay responsive
/// when the next protocol deadline is farther out.
const LISTENER_IDLE: Duration = Duration::from_millis(5);

pub(crate) struct SrtServer {
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    engine: Arc<MediaEngine>,
    security: Arc<IngestSecurityService>,
    ingest_policy_store: Arc<SrtIngestPolicyStore>,
}

impl SrtServer {
    pub fn new(
        pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
        engine: Arc<MediaEngine>,
        security: Arc<IngestSecurityService>,
        ingest_policy_store: Arc<SrtIngestPolicyStore>,
    ) -> Self {
        Self {
            pipeline_access,
            engine,
            security,
            ingest_policy_store,
        }
    }

    pub async fn run(self: Arc<Self>, port: u16) {
        let bind = match format!("0.0.0.0:{port}").parse() {
            Ok(bind) => bind,
            Err(error) => {
                error!(port, %error, "invalid SRT listener address");
                return;
            }
        };
        let prepared = match ListenerConfig::builder(bind)
            .topology(ListenerTopology::PerPort)
            .bonded_inputs(BondedInputPolicy::Accept)
            .build()
            .and_then(|config| config.prepare(RuntimeFlavor::Tokio))
        {
            Ok(prepared) => prepared,
            Err(error) => {
                error!(port, %error, "failed to prepare srt-rs listener");
                return;
            }
        };
        let mut sockets = match prepared.bind_sockets() {
            Ok(sockets) => sockets,
            Err(error) => {
                error!(port, %error, "failed to bind srt-rs listener");
                return;
            }
        };
        let Some(socket) = sockets.pop() else {
            error!(port, "srt-rs listener produced no UDP socket");
            return;
        };
        let socket = match UdpSocket::from_std(socket) {
            Ok(socket) => socket,
            Err(error) => {
                error!(port, %error, "failed to adopt SRT listener UDP socket into Tokio");
                return;
            }
        };

        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_hook = shutdown.clone();
        self.engine.register_listener_shutdown(move || {
            shutdown_hook.store(true, std::sync::atomic::Ordering::Release);
        });

        let mut peers = prepared.peer_table();
        self.engine
            .listener_stats_handle()
            .bonding_available
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let admission = prepared.admission_options();
        let telemetry = IngressTelemetry::default();
        let mut peer_sessions = HashMap::new();
        let mut events = Vec::new();
        let mut outbound = Vec::new();
        let mut recv_batch = RecvBatch::new();
        let mut waiter = match HighResWaiter::<()>::new() {
            Ok(waiter) => waiter,
            Err(error) => {
                error!(port, %error, "failed to create SRT listener HighResWaiter");
                return;
            }
        };
        if let Err(error) = waiter.register((), socket.as_raw_fd()) {
            error!(port, %error, "failed to register SRT listener with HighResWaiter");
            return;
        }
        let mut due = Vec::new();
        let mut ready = Vec::new();

        info!(port, "SRT listener ready (srt-rs/Tokio)");
        while !shutdown.load(std::sync::atomic::Ordering::Acquire) {
            let wait = listener_wait_duration(&mut peers, timestamp_now());
            match tokio::task::block_in_place(|| {
                park_listener(&mut waiter, &mut due, &mut ready, wait)
            }) {
                Ok(socket_ready) => {
                    if socket_ready {
                        let now = timestamp_now();
                        // HighResWaiter observed the raw fd. `drain_readable`
                        // requires Tokio READABLE, which is still unset here,
                        // so handshake datagrams would be dropped as WouldBlock.
                        match drain_woken_listener(
                            &socket,
                            &mut recv_batch,
                            RecvBudget::default(),
                            |addr, data| {
                                let Some(peer) = addr else {
                                    return;
                                };
                                let policy_store = self.ingest_policy_store.clone();
                                let _ = peers.admit_with_resolver(
                                    peer,
                                    data,
                                    now,
                                    &admission,
                                    0,
                                    1,
                                    &telemetry,
                                    move |request| resolve_listener_policy(&policy_store, request),
                                );
                            },
                        ) {
                            Ok(_) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(error) => {
                                warn!(%error, "SRT listener receive failed");
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    warn!(%error, "SRT listener wait failed");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }

            let now = timestamp_now();
            close_deleted_srt_publishers(&self.engine, &mut peers, &mut peer_sessions, now).await;
            drive_srt_readers(&self.engine, &mut peers, &mut peer_sessions, now).await;
            peers.poll_outbound(now, &mut outbound);
            for (peer, packet) in outbound.drain(..) {
                let _ = socket.send_to(&packet, peer).await;
            }
            peers.poll_events(&mut events);
            for AdmissionEvent {
                representative_peer: peer,
                logical_peer,
                event,
            } in events.drain(..)
            {
                self.handle_peer_event(&mut peers, &mut peer_sessions, peer, logical_peer, event)
                    .await;
            }
        }

        for (_logical_peer, session) in peer_sessions.drain() {
            if let RustSrtSession::Publish(publisher) = session {
                self.finish_publisher(*publisher).await;
            }
        }
        self.engine
            .listener_stats_handle()
            .bonding_available
            .store(false, std::sync::atomic::Ordering::Relaxed);
        info!(port, "SRT listener stopped");
    }

    async fn handle_peer_event(
        &self,
        peers: &mut PeerTable,
        sessions: &mut HashMap<LogicalPeerId, RustSrtSession>,
        peer: SocketAddr,
        logical_peer: LogicalPeerId,
        event: ConnectionEvent,
    ) {
        match event {
            ConnectionEvent::Connected => {
                let stream_id = peers
                    .logical_peer(&logical_peer)
                    .and_then(|entry| entry.stream_id().map(str::to_owned))
                    .unwrap_or_default();
                let parsed = parse_srt_stream_id(&stream_id);
                let client_ip = peer.ip().to_string();
                let access_mode = match parsed.mode {
                    SrtConnectionMode::Publish => PipelineAccessMode::SrtPublish,
                    SrtConnectionMode::Read => PipelineAccessMode::SrtRead,
                };
                if self
                    .security
                    .is_ip_banned_for(RateLimitScope::SrtPublish, &client_ip)
                    .or_else(|| {
                        self.security
                            .is_ip_banned_for(RateLimitScope::SrtRead, &client_ip)
                    })
                    .is_some()
                {
                    let _ = peers.remove(logical_peer);
                    return;
                }
                let pipeline = match self
                    .pipeline_access
                    .authenticate(access_mode, &parsed.stream_key, &client_ip)
                    .await
                {
                    Ok(pipeline) => pipeline,
                    Err(error) => {
                        warn!(peer = %peer, error = ?error, "rejecting unauthorized SRT stream");
                        let _ = peers.remove(logical_peer);
                        return;
                    }
                };
                match parsed.mode {
                    SrtConnectionMode::Publish => {
                        match self
                            .start_publisher(peer, pipeline, parsed.stream_key)
                            .await
                        {
                            Ok(session) => {
                                sessions.insert(
                                    logical_peer,
                                    RustSrtSession::Publish(Box::new(session)),
                                );
                            }
                            Err(error) => {
                                warn!(peer = %peer, %error, "rejecting SRT publisher");
                                let _ = peers.remove(logical_peer);
                            }
                        }
                    }
                    SrtConnectionMode::Read => match self.start_reader(&pipeline.id).await {
                        Ok(reader) => {
                            sessions.insert(logical_peer, RustSrtSession::Read(reader));
                        }
                        Err(error) => {
                            warn!(peer = %peer, %error, "rejecting SRT reader");
                            let _ = peers.remove(logical_peer);
                        }
                    },
                }
            }
            ConnectionEvent::DataReceived { payload, .. } => {
                if let Some(RustSrtSession::Publish(publisher)) = sessions.get_mut(&logical_peer) {
                    publisher.accept_payload(&self.engine, payload).await;
                }
            }
            ConnectionEvent::Disconnected { reason } => {
                if let Some(session) = sessions.remove(&logical_peer)
                    && let RustSrtSession::Publish(publisher) = session
                {
                    self.finish_publisher(*publisher).await;
                }
                info!(peer = %peer, %reason, "SRT peer disconnected");
                let _ = peers.remove(logical_peer);
            }
            ConnectionEvent::StateChanged(_)
            | ConnectionEvent::Error(_)
            | ConnectionEvent::KeyRefreshNeeded { .. } => {}
        }
    }

    async fn start_publisher(
        &self,
        peer: SocketAddr,
        pipeline: crate::media::ingest_auth::AuthenticatedPipeline,
        stream_key: String,
    ) -> Result<RustSrtPublisher, String> {
        let ring_buffer = self.engine.get_or_create_pipeline(&pipeline.id).await;
        let registration = self
            .engine
            .try_register_pipeline_input_attempt(
                &pipeline.id,
                &pipeline.input_id,
                &stream_key,
                "srt",
                pipeline.selected,
            )
            .await
            .ok_or_else(|| "duplicate SRT publisher".to_string())?;
        self.engine
            .update_ingest_session_meta(
                &pipeline.id,
                &registration,
                None,
                None,
                Some(peer.to_string()),
            )
            .await;
        let Some((bytes_received, ingest_metrics, last_progress_ms, keyframe_times)) = self
            .engine
            .with_ingest_session(&registration, |ingest| {
                (
                    ingest.bytes_received.clone(),
                    ingest.metrics.clone(),
                    ingest.last_progress_ms.clone(),
                    ingest.keyframe_times.clone(),
                )
            })
            .await
        else {
            self.engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            return Err("SRT ingest session disappeared during registration".to_string());
        };
        Ok(RustSrtPublisher {
            pipeline_id: pipeline.id,
            registration,
            ring_buffer,
            demuxer: crate::media::mpegts::TsDemuxer::new(),
            timestamp_mapper: InputTimestampMapper::default(),
            standby_gop: StandbyGopCache::default(),
            packets: Vec::with_capacity(16),
            keyframe_times,
            bytes_received,
            ingest_metrics,
            last_progress_ms,
            probe_sent: false,
            closing: false,
        })
    }

    async fn start_reader(&self, pipeline_id: &str) -> Result<RustSrtReader, String> {
        if !self
            .engine
            .ingests
            .active
            .read()
            .await
            .contains_key(pipeline_id)
        {
            return Err("no active ingest".to_string());
        }
        let ring = self.engine.get_or_create_pipeline(pipeline_id).await;
        let muxer = self
            .engine
            .get_or_create_ts_muxer_stage(pipeline_id, "play", ring)
            .await;
        Ok(RustSrtReader {
            pipeline_id: pipeline_id.to_string(),
            closing: false,
            muxer: TsChunkReader::new(format!("srt_play:{pipeline_id}"), &muxer),
            packets: Vec::with_capacity(crate::media::ring_buffer::MEDIA_PULL_BURST_PACKETS),
            pending: VecDeque::new(),
        })
    }

    async fn finish_publisher(&self, mut publisher: RustSrtPublisher) {
        publisher.demuxer.flush();
        if publisher.demuxer.drain_into(&mut publisher.packets) > 0 {
            ingest_packets::forward_ingest_packets(
                &mut publisher.packets,
                &publisher.ring_buffer,
                &publisher.registration,
                &mut publisher.timestamp_mapper,
                &mut publisher.standby_gop,
                None,
            );
        }
        self.engine
            .record_ingest_disconnect_if_current(
                &publisher.pipeline_id,
                &publisher.registration,
                Some("disconnect"),
                Some("SRT peer disconnected".to_string()),
                false,
            )
            .await;
        self.engine
            .unregister_ingest_if_current(&publisher.pipeline_id, &publisher.registration)
            .await;
    }
}

enum RustSrtSession {
    Publish(Box<RustSrtPublisher>),
    Read(RustSrtReader),
}

struct RustSrtPublisher {
    pipeline_id: String,
    registration: crate::media::engine::IngestRegistration,
    ring_buffer: Arc<RingBuffer>,
    demuxer: crate::media::mpegts::TsDemuxer,
    timestamp_mapper: InputTimestampMapper,
    standby_gop: StandbyGopCache,
    packets: Vec<crate::media::packet::MediaPacket>,
    keyframe_times: Arc<Mutex<Vec<i64>>>,
    bytes_received: Arc<std::sync::atomic::AtomicU64>,
    ingest_metrics: Arc<crate::media::stage_metrics::StageMetrics>,
    last_progress_ms: Arc<std::sync::atomic::AtomicU64>,
    probe_sent: bool,
    closing: bool,
}

impl RustSrtPublisher {
    async fn accept_payload(&mut self, engine: &MediaEngine, payload: Bytes) {
        self.demuxer.feed(payload.as_ref());
        if self.demuxer.drain_into(&mut self.packets) > 0 {
            ingest_packets::forward_ingest_packets(
                &mut self.packets,
                &self.ring_buffer,
                &self.registration,
                &mut self.timestamp_mapper,
                &mut self.standby_gop,
                Some(&self.keyframe_times),
            );
        }
        if !self.probe_sent
            && let Some(probe) = self.demuxer.take_probe()
        {
            self.probe_sent = true;
            let video_fps = probe.video.as_ref().map(|video| video.fps).unwrap_or(30.0);
            let audio_track_count = probe.audio_tracks.len();
            let first_audio = probe.audio_tracks.first().cloned();
            let selected_video_track_index = probe.video.as_ref().map(|_| 0);
            engine
                .update_ingest_session_meta(
                    &self.pipeline_id,
                    &self.registration,
                    probe.video,
                    first_audio,
                    None,
                )
                .await;
            engine
                .update_ingest_session_video_track_selection(
                    &self.registration,
                    probe.video_track_count,
                    selected_video_track_index,
                )
                .await;
            if !probe.audio_tracks.is_empty() {
                engine
                    .update_ingest_session_audio_tracks(
                        &self.pipeline_id,
                        &self.registration,
                        probe.audio_tracks,
                    )
                    .await;
            }
            if engine
                .is_ingest_session_selected(&self.pipeline_id, &self.registration)
                .await
                && let Some(new_ring) = engine
                    .adapt_pipeline_ring(&self.pipeline_id, video_fps, audio_track_count)
                    .await
            {
                self.ring_buffer = new_ring;
            }
        }
        self.bytes_received
            .fetch_add(payload.len() as u64, std::sync::atomic::Ordering::Relaxed);
        self.ingest_metrics.record_in(payload.len() as u64);
        self.last_progress_ms.store(
            MediaEngine::now_epoch_ms(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

struct RustSrtReader {
    pipeline_id: String,
    closing: bool,
    muxer: TsChunkReader,
    packets: Vec<Arc<crate::media::packet::MediaPacket>>,
    pending: VecDeque<Bytes>,
}

async fn close_deleted_srt_publishers(
    engine: &MediaEngine,
    peers: &mut PeerTable,
    sessions: &mut HashMap<LogicalPeerId, RustSrtSession>,
    now: Timestamp,
) {
    let live_pipelines = engine.ingests.pipelines.read().await;
    let publisher_peers: Vec<_> = sessions
        .iter()
        .filter_map(|(peer, session)| match session {
            RustSrtSession::Publish(publisher) => Some((*peer, publisher.pipeline_id.clone())),
            RustSrtSession::Read(_) => None,
        })
        .collect();

    for (peer, pipeline_id) in publisher_peers {
        if live_pipelines.contains_key(&pipeline_id) {
            continue;
        }
        let Some(RustSrtSession::Publish(publisher)) = sessions.get_mut(&peer) else {
            continue;
        };
        if publisher.closing {
            continue;
        }
        let Some(mut entry) = peers.logical_peer_mut(&peer) else {
            continue;
        };
        // A sink pipeline can disappear while its SRT publisher connection is
        // still healthy. Close that connection immediately so the producing
        // egress observes a normal peer shutdown and enters retry/cleanup.
        entry.disconnect(now);
        publisher.closing = true;
    }
}

async fn drive_srt_readers(
    engine: &MediaEngine,
    peers: &mut PeerTable,
    sessions: &mut HashMap<LogicalPeerId, RustSrtSession>,
    now: Timestamp,
) {
    // The pipeline ring is removed by pipeline deletion immediately, while
    // the active ingest entry intentionally remains until the peer teardown
    // completes. Use ring ownership as the lifecycle signal so a deleted
    // reader target is closed instead of waiting for SRT inactivity timeout.
    let live_pipelines = engine.ingests.pipelines.read().await;
    let reader_peers: Vec<_> = sessions
        .iter_mut()
        .filter_map(|(peer, session)| match session {
            RustSrtSession::Read(reader) => Some((*peer, reader)),
            RustSrtSession::Publish(_) => None,
        })
        .collect();
    for (peer, reader) in reader_peers {
        if !live_pipelines.contains_key(&reader.pipeline_id) {
            if !reader.closing
                && let Some(mut entry) = peers.logical_peer_mut(&peer)
            {
                // A target pipeline can be deleted independently of the SRT
                // socket. Send a protocol shutdown so the remote egress sees
                // the disappearance promptly instead of waiting for idle
                // timeout/retry handling.
                entry.disconnect(now);
                reader.closing = true;
            }
            continue;
        }
        if reader.closing {
            continue;
        }
        if reader.pending.is_empty() {
            reader.packets.clear();
            if reader
                .muxer
                .pull_burst(
                    &mut reader.packets,
                    crate::media::ring_buffer::MEDIA_PULL_BURST_PACKETS,
                )
                .unwrap_or(0)
                > 0
            {
                for packet in &reader.packets {
                    for fragment in packet.payload.chunks(SRT_MESSAGE_PAYLOAD_MAX) {
                        if !fragment.is_empty() {
                            reader.pending.push_back(Bytes::copy_from_slice(fragment));
                        }
                    }
                }
            }
        }
        while let Some(payload) = reader.pending.front().cloned() {
            let Some(mut entry) = peers.logical_peer_mut(&peer) else {
                break;
            };
            let sent = match entry.send(&payload, now) {
                Ok(_) => {
                    reader.pending.pop_front();
                    true
                }
                Err(error) => {
                    warn!(
                        peer = ?peer,
                        error = ?error,
                        payload_len = payload.len(),
                        "SRT reader send failed"
                    );
                    false
                }
            };
            if !sent
                || !peers
                    .logical_peer_mut(&peer)
                    .is_some_and(|mut entry| entry.can_send())
            {
                break;
            }
        }
    }
}

fn resolve_listener_policy(
    store: &SrtIngestPolicyStore,
    request: &srt_transport::AdmissionRequest,
) -> AdmissionResolution {
    let stream_id = request
        .claimed_identity
        .stream_id
        .as_deref()
        .unwrap_or_default();
    let parsed = parse_srt_stream_id(stream_id);
    if parsed.stream_key.is_empty()
        || !matches!(
            parsed.mode,
            SrtConnectionMode::Publish | SrtConnectionMode::Read
        )
    {
        return AdmissionResolution::Reject {
            reason: RejectionReason::BAD_MODE,
        };
    }
    let Some(resolved) = store.resolved_policy(&parsed.stream_key) else {
        return AdmissionResolution::Reject {
            reason: RejectionReason::UNAUTHORIZED,
        };
    };
    let mut policy = ListenerPeerPolicy {
        latency: PolicyOverride::Set(Duration::from_millis(resolved.latency_ms.max(0) as u64)),
        encryption: PolicyOverride::Set(None),
        ..ListenerPeerPolicy::default()
    };
    if let ResolvedSrtCrypto::Encrypted {
        passphrase,
        pbkeylen,
    } = resolved.crypto
    {
        let Some(key_length) = shiguredo_srt::KeyLength::from_len(pbkeylen as usize) else {
            return AdmissionResolution::Reject {
                reason: RejectionReason::BAD_REQUEST,
            };
        };
        let Ok(encryption) = ListenerEncryptionConfig::new(passphrase, key_length) else {
            return AdmissionResolution::Reject {
                reason: RejectionReason::BAD_REQUEST,
            };
        };
        policy.encryption = PolicyOverride::Set(Some(encryption));
    }
    AdmissionResolution::Configure(policy)
}

fn listener_wait_duration(peers: &mut PeerTable, now: Timestamp) -> Duration {
    Duration::from_micros(
        peers
            .time_until_next_deadline(now, listener_idle_micros())
            .min(listener_idle_micros()),
    )
}

fn listener_idle_micros() -> u64 {
    u64::try_from(LISTENER_IDLE.as_micros()).unwrap_or(u64::MAX)
}

fn park_listener(
    waiter: &mut HighResWaiter<()>,
    due: &mut Vec<()>,
    ready: &mut Vec<()>,
    wait: Duration,
) -> std::io::Result<bool> {
    waiter.set_deadline((), MonotonicDeadline::after(wait));
    waiter.wait(due, ready)?;
    Ok(!ready.is_empty())
}

/// Drain a socket that `HighResWaiter` already reported readable.
///
/// Must not use [`srt_transport::tokio_transport::drain_readable`]: that
/// helper's `try_io(READABLE)` returns WouldBlock unless Tokio itself saw
/// the wake. After a waiter park the kernel queue is full and Tokio is
/// not, which is the post-#153 MSR sink/ingress handshake stall.
fn drain_woken_listener(
    socket: &UdpSocket,
    recv_batch: &mut RecvBatch,
    budget: RecvBudget,
    on_datagram: impl FnMut(Option<std::net::SocketAddr>, &[u8]),
) -> std::io::Result<srt_transport::RecvDrainReport> {
    srt_transport::drain_recv_fd(socket.as_raw_fd(), recv_batch, budget, on_datagram)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_peer_table_caps_listener_wait_at_idle() {
        let mut peers = PeerTable::new();
        assert_eq!(
            listener_wait_duration(&mut peers, timestamp_now()),
            LISTENER_IDLE
        );
    }

    fn pending_datagram_socket() -> std::net::UdpSocket {
        let receiver = std::net::UdpSocket::bind("127.0.0.1:0").expect("receiver binds");
        receiver
            .set_nonblocking(true)
            .expect("receiver is nonblocking");
        let dest = receiver.local_addr().expect("receiver address");
        let sender = std::net::UdpSocket::bind("127.0.0.1:0").expect("sender binds");
        sender.send_to(b"ping", dest).expect("send datagram");
        receiver
    }

    fn with_woken_listener(
        test: impl FnOnce(UdpSocket, &mut HighResWaiter<()>, &mut Vec<()>, &mut Vec<()>),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("Tokio runtime builds");
        let receiver = pending_datagram_socket();
        runtime.block_on(async {
            let sock = UdpSocket::from_std(receiver).expect("tokio adopts the socket");
            let mut waiter = HighResWaiter::<()>::new().expect("waiter");
            waiter
                .register((), sock.as_raw_fd())
                .expect("register listener fd");
            let mut due = Vec::new();
            let mut ready = Vec::new();
            assert!(
                park_listener(&mut waiter, &mut due, &mut ready, LISTENER_IDLE).expect("wait"),
                "listener fd should be ready after a datagram"
            );
            test(sock, &mut waiter, &mut due, &mut ready);
        });
    }

    #[test]
    fn high_res_waiter_wakes_the_listener_socket() {
        with_woken_listener(|_, _, _, _| {});
    }

    #[test]
    fn woken_listener_drains_without_tokio_readable() {
        with_woken_listener(|sock, _, _, _| {
            let mut batch = RecvBatch::new();
            let mut got = Vec::new();
            let report =
                drain_woken_listener(&sock, &mut batch, RecvBudget::default(), |_, data| {
                    got.push(data.to_vec())
                })
                .expect("drain after waiter");
            assert_eq!(report.datagrams, 1);
            assert_eq!(got, [b"ping".to_vec()]);
        });
    }

    #[test]
    fn drain_readable_misses_a_waiter_wake_without_tokio_readable() {
        with_woken_listener(|sock, _, _, _| {
            let mut batch = RecvBatch::new();
            let mut got = Vec::new();
            let report = srt_transport::tokio_transport::drain_readable(
                &sock,
                &mut batch,
                RecvBudget::default(),
                |_, data| got.push(data.to_vec()),
            )
            .expect("drain_readable after waiter");
            assert_eq!(
                report.datagrams, 0,
                "Tokio try_io is unset after HighResWaiter; this is the #153 MSR stall"
            );
            assert!(report.would_block);
            assert!(got.is_empty());
        });
    }
}
