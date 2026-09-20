//! Tokio listener, admission, and media-session lifecycle for srt-rs ingress.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use srt_transport::advanced::admission::LogicalPeerId;
use tracing::{error, info, warn};

use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{PipelineAccessAuthenticator, PipelineAccessMode};
use crate::media::input_gate::InputTimestampMapper;
use crate::media::ring_buffer::RingBuffer;
use crate::media::security::{IngestSecurityService, RateLimitScope};
use crate::media::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};

use crate::media::standby_gop::StandbyGopCache;
use crate::media::ts_chunk_ring::TsChunkReader;

pub(crate) use super::srt_policy::SrtIngestPolicyStore;

use super::ingress_admission::ReceiverGroupId;
use super::ingress_owner::{
    INGRESS_COMMAND_CAPACITY, INGRESS_EVENT_CAPACITY, IngressCommand, IngressConfig,
    SrtIngressEvent, SrtIngressHandle,
};

#[path = "ingest_packets.rs"]
mod ingest_packets;

const SRT_MESSAGE_PAYLOAD_MAX: usize = 1316;
/// Upper bound for listener parks so reader pull and shutdown stay responsive
/// when no Owner event arrives.
const LISTENER_IDLE: Duration = Duration::from_millis(5);
/// Owner events handled per Tokio pass before readers and deletion checks run.
const EVENTS_PER_PASS: usize = 64;
/// Send commands one reader may queue per Tokio pass, so one busy reader
/// cannot monopolize the command bridge.
const READER_SENDS_PER_PASS: usize = 32;

/// Tokio's end of the ingress Owner: the bounded command bridge plus the
/// disconnects waiting for bridge room. Sessions are addressed only by
/// `LogicalPeerId`; the protocol state they name lives on the owner thread.
struct Ingress {
    handle: SrtIngressHandle,
    pending_disconnects: VecDeque<LogicalPeerId>,
}

impl Ingress {
    /// Ask the Owner to disconnect (and then retire) a peer. Never lost: a
    /// full bridge leaves it queued for the next pass.
    fn disconnect(&mut self, peer: LogicalPeerId) {
        self.pending_disconnects.push_back(peer);
        self.flush_disconnects();
    }

    fn flush_disconnects(&mut self) {
        while let Some(peer) = self.pending_disconnects.pop_front() {
            if let Err(IngressCommand::Disconnect { logical_peer }) = self
                .handle
                .try_send(IngressCommand::Disconnect { logical_peer: peer })
            {
                self.pending_disconnects.push_front(logical_peer);
                break;
            }
        }
    }
}

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
        let handle = match SrtIngressHandle::start(IngressConfig {
            bind,
            policy_store: self.ingest_policy_store.clone(),
            receiver_group: ReceiverGroupId::generate(),
            stats: self.engine.listener_stats_handle(),
            command_capacity: INGRESS_COMMAND_CAPACITY,
            event_capacity: INGRESS_EVENT_CAPACITY,
        })
        .await
        {
            Ok(handle) => handle,
            Err(error) => {
                error!(port, %error, "failed to start the SRT ingress owner");
                return;
            }
        };
        let mut ingress = Ingress {
            handle,
            pending_disconnects: VecDeque::new(),
        };

        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let shutdown_hook = shutdown.clone();
        self.engine.register_listener_shutdown(move || {
            shutdown_hook.store(true, std::sync::atomic::Ordering::Release);
        });

        self.engine
            .listener_stats_handle()
            .bonding_available
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // Protocol state (the Compio Owner, its PeerTable, timers, replies)
        // lives on the ingress owner thread. Tokio keeps session and media
        // lifecycle keyed by `LogicalPeerId` and reaches the protocol only
        // through bounded commands. There is no second protocol table here.
        let mut peer_sessions = HashMap::new();

        info!(port, bind = %ingress.handle.local_addr(), "SRT listener ready (srt-rs compio Owner ingress)");
        let mut owner_fault = None;
        'listener: while !shutdown.load(std::sync::atomic::Ordering::Acquire) {
            ingress.flush_disconnects();
            close_deleted_srt_publishers(&self.engine, &mut ingress, &mut peer_sessions).await;
            drive_srt_readers(&self.engine, &mut ingress, &mut peer_sessions).await;
            let first = tokio::time::timeout(LISTENER_IDLE, ingress.handle.events.recv()).await;
            let mut next = match first {
                Ok(Some(event)) => Some(event),
                Ok(None) => {
                    owner_fault = Some("the SRT ingress owner thread stopped".to_string());
                    break;
                }
                Err(_) => None,
            };
            let mut handled = 0;
            while let Some(event) = next.take() {
                if let SrtIngressEvent::Fault { detail } = &event {
                    owner_fault = Some(detail.clone());
                    break 'listener;
                }
                self.handle_ingress_event(&mut ingress, &mut peer_sessions, event)
                    .await;
                handled += 1;
                if handled >= EVENTS_PER_PASS {
                    break;
                }
                next = ingress.handle.events.try_recv().ok();
            }
            if ingress.handle.is_closed() {
                owner_fault = Some("the SRT ingress owner thread stopped".to_string());
                break;
            }
        }

        // Ask every live peer to close orderly, then stop the owner and report
        // its truthful verdict before the listener is marked stopped.
        for logical_peer in peer_sessions.keys().copied().collect::<Vec<_>>() {
            ingress.disconnect(logical_peer);
        }
        let exit = ingress.handle.shutdown().await;
        if let Some(fault) = owner_fault.as_ref().or(exit.fault.as_ref()) {
            error!(port, fault = %fault, quiescent = exit.quiescent, "SRT listener stopped after an owner fault");
        } else if !exit.quiescent {
            warn!(
                port,
                "SRT ingress owner did not reach quiescence at shutdown"
            );
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
        info!(port, quiescent = exit.quiescent, "SRT listener stopped");
    }

    async fn handle_ingress_event(
        &self,
        ingress: &mut Ingress,
        sessions: &mut HashMap<LogicalPeerId, RustSrtSession>,
        event: SrtIngressEvent,
    ) {
        match event {
            SrtIngressEvent::Fault { .. } => {}
            SrtIngressEvent::Connected {
                peer,
                logical_peer,
                stream_id,
            } => {
                self.handle_connected(ingress, sessions, peer, logical_peer, stream_id)
                    .await;
            }
            SrtIngressEvent::Media {
                logical_peer,
                payload,
            } => {
                if let Some(RustSrtSession::Publish(publisher)) = sessions.get_mut(&logical_peer) {
                    publisher.accept_payload(&self.engine, payload).await;
                }
            }
            SrtIngressEvent::Disconnected {
                peer,
                logical_peer,
                reason,
            } => {
                if let Some(session) = sessions.remove(&logical_peer)
                    && let RustSrtSession::Publish(publisher) = session
                {
                    self.finish_publisher(*publisher).await;
                }
                // The owner retires the terminal peer itself; nothing to remove.
                info!(peer = %peer, %reason, "SRT peer disconnected");
            }
        }
    }

    async fn handle_connected(
        &self,
        ingress: &mut Ingress,
        sessions: &mut HashMap<LogicalPeerId, RustSrtSession>,
        peer: SocketAddr,
        logical_peer: LogicalPeerId,
        stream_id: String,
    ) {
        {
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
                ingress.disconnect(logical_peer);
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
                    ingress.disconnect(logical_peer);
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
                            sessions
                                .insert(logical_peer, RustSrtSession::Publish(Box::new(session)));
                        }
                        Err(error) => {
                            warn!(peer = %peer, %error, "rejecting SRT publisher");
                            ingress.disconnect(logical_peer);
                        }
                    }
                }
                SrtConnectionMode::Read => match self.start_reader(&pipeline.id).await {
                    Ok(reader) => {
                        sessions.insert(logical_peer, RustSrtSession::Read(reader));
                    }
                    Err(error) => {
                        warn!(peer = %peer, %error, "rejecting SRT reader");
                        ingress.disconnect(logical_peer);
                    }
                },
            }
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
    ingress: &mut Ingress,
    sessions: &mut HashMap<LogicalPeerId, RustSrtSession>,
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
        // A sink pipeline can disappear while its SRT publisher connection is
        // still healthy. Close that connection immediately (an Owner command
        // against the real logical peer) so the producing egress observes a
        // normal peer shutdown and enters retry/cleanup.
        ingress.disconnect(peer);
        publisher.closing = true;
    }
}

async fn drive_srt_readers(
    engine: &MediaEngine,
    ingress: &mut Ingress,
    sessions: &mut HashMap<LogicalPeerId, RustSrtSession>,
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
            if !reader.closing {
                // A target pipeline can be deleted independently of the SRT
                // socket. Ask the Owner to close the real peer so the remote
                // egress sees the disappearance promptly instead of waiting
                // for idle timeout/retry handling.
                ingress.disconnect(peer);
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
        // A fragment leaves `pending` only once the bounded command bridge has
        // accepted it. A full bridge stops the pull for this reader (the next
        // burst is not fetched while fragments wait), so nothing is dropped.
        submit_pending(&mut reader.pending, READER_SENDS_PER_PASS, |payload| {
            ingress
                .handle
                .try_send(IngressCommand::Send {
                    logical_peer: peer,
                    payload,
                })
                .map_err(|command| match command {
                    IngressCommand::Send { payload, .. } => payload,
                    IngressCommand::Disconnect { .. } | IngressCommand::Shutdown => {
                        unreachable!("only a Send command was offered")
                    }
                })
        });
    }
}

/// Offer up to `limit` pending fragments to a bounded sink. A fragment leaves
/// `pending` only once the sink has accepted it, so a full bridge stops the
/// pass with nothing lost and order preserved. Returns the accepted count.
pub(super) fn submit_pending(
    pending: &mut VecDeque<Bytes>,
    limit: usize,
    mut offer: impl FnMut(Bytes) -> Result<(), Bytes>,
) -> usize {
    let mut accepted = 0;
    while accepted < limit {
        let Some(payload) = pending.front().cloned() else {
            break;
        };
        if offer(payload).is_err() {
            break;
        }
        pending.pop_front();
        accepted += 1;
    }
    accepted
}
