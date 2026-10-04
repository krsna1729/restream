//! Tokio session lifecycle for srt-rs ingress: admission, publisher and reader
//! registration, the one-time stream probe, disconnect bookkeeping, receive
//! quality and deletion checks. Media never passes through here: it runs to
//! completion on the ingress Owner thread (`ingress_media`).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::{error, info, warn};

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::ingest_auth::{PipelineAccessAuthenticator, PipelineAccessMode};
use crate::media::input_gate::InputTimestampMapper;
use crate::media::mpegts::DemuxProbe;
use crate::media::ring_buffer::RingBuffer;
use crate::media::security::{IngestSecurityService, RateLimitScope};
use crate::media::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};
use crate::media::standby_gop::StandbyGopCache;
use crate::media::ts_chunk_ring::TsChunkReader;

pub(crate) use super::srt_policy::SrtIngestPolicyStore;

use super::ingress_admission::ReceiverGroupId;
use super::ingress_media::{SrtPublisherMedia, SrtReaderMedia};
use super::ingress_owner::{
    INGRESS_COMMAND_CAPACITY, INGRESS_EVENT_CAPACITY, INGRESS_TELEMETRY_CAPACITY, IngressCommand,
    IngressConfig, IngressPeer, SrtIngressEvent, SrtIngressHandle,
};
use super::ingress_quality::{QualityFold, QualitySample};

/// How often deleted pipelines are checked and the shutdown flag is read.
/// Lifecycle only: media does not wait on it.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(100);

/// Tokio's end of the ingress Owners: the bounded command bridges plus the
/// commands waiting for bridge room. Sessions are addressed only by
/// `IngressPeer` (Owner index + `LogicalPeerId`); the protocol and media state
/// they name live on that Owner's thread.
struct Ingress {
    handle: SrtIngressHandle,
    pending_commands: VecDeque<IngressCommand>,
}

impl Ingress {
    /// Queue a command for the Owner. Never lost: a full bridge leaves it
    /// queued (in order) for the next flush.
    fn command(&mut self, command: IngressCommand) {
        self.pending_commands.push_back(command);
        self.flush_commands();
    }

    fn disconnect(&mut self, peer: IngressPeer) {
        self.command(IngressCommand::Disconnect { logical_peer: peer });
    }

    fn flush_commands(&mut self) {
        while let Some(command) = self.pending_commands.pop_front() {
            if let Err(command) = self.handle.try_send(command) {
                self.pending_commands.push_front(command);
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

/// What Tokio keeps about a session: identity for bookkeeping, never media.
enum Session {
    Publish(PublisherSession),
    Read { pipeline_id: String, closing: bool },
}

pub(super) struct PublisherSession {
    pipeline_id: String,
    registration: IngestRegistration,
    closing: bool,
    /// Tracks the last Owner observation, so rates use Owner-to-Owner intervals.
    quality_fold: QualityFold,
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
            telemetry_capacity: INGRESS_TELEMETRY_CAPACITY,
            owners: self.engine.config.srt_ingress_owners,
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
            pending_commands: VecDeque::new(),
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
        let mut sessions: HashMap<IngressPeer, Session> = HashMap::new();

        info!(port, bind = %ingress.handle.local_addr(), "SRT listener ready (srt-rs compio Owner ingress)");
        let mut owner_fault = None;
        let mut housekeeping = tokio::time::interval(HOUSEKEEPING_INTERVAL);
        housekeeping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                event = ingress.handle.events.recv() => match event {
                    Some(SrtIngressEvent::Fault { detail }) => {
                        owner_fault = Some(detail);
                        break;
                    }
                    Some(event) => self.handle_ingress_event(&mut ingress, &mut sessions, event).await,
                    None => {
                        owner_fault = Some("the SRT ingress owner thread stopped".to_string());
                        break;
                    }
                },
                sample = ingress.handle.telemetry.recv() => {
                    if let Some(sample) = sample {
                        self.fold_telemetry(&mut sessions, sample).await;
                    }
                }
                _ = housekeeping.tick() => {
                    if shutdown.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }
                    close_deleted_sessions(&self.engine, &mut ingress, &mut sessions).await;
                    if ingress.handle.is_closed() {
                        owner_fault = Some("the SRT ingress owner thread stopped".to_string());
                        break;
                    }
                }
            }
            ingress.flush_commands();
        }

        // Ask every live peer to close orderly, then stop the owner (which
        // flushes every publisher's last media) and report its truthful
        // verdict before the listener is marked stopped.
        for logical_peer in sessions.keys().copied().collect::<Vec<_>>() {
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
        for (_logical_peer, session) in sessions.drain() {
            if let Session::Publish(publisher) = session {
                self.finish_publisher(publisher).await;
            }
        }
        self.engine
            .listener_stats_handle()
            .bonding_available
            .store(false, std::sync::atomic::Ordering::Relaxed);
        info!(port, quiescent = exit.quiescent, "SRT listener stopped");
    }

    /// Fold one Owner-stamped receive-quality sample into its publisher's
    /// ingest snapshot (generation-safe through the registration). A sample
    /// for a peer with no publisher session is dropped; the next one (about a
    /// second later) replaces it.
    async fn fold_telemetry(
        &self,
        sessions: &mut HashMap<IngressPeer, Session>,
        QualitySample { peer, observation }: QualitySample,
    ) {
        let Some(Session::Publish(publisher)) = sessions.get_mut(&peer) else {
            return;
        };
        let Some(quality) = publisher.quality_fold.fold(observation) else {
            return;
        };
        self.engine
            .update_ingest_session_quality(&publisher.registration, quality)
            .await;
    }

    async fn handle_ingress_event(
        &self,
        ingress: &mut Ingress,
        sessions: &mut HashMap<IngressPeer, Session>,
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
            SrtIngressEvent::Probe {
                logical_peer,
                probe,
            } => {
                let ring = match sessions.get(&logical_peer) {
                    Some(Session::Publish(publisher)) => self.apply_probe(publisher, probe).await,
                    _ => None,
                };
                // Always answer: the Owner holds the publisher's packets until
                // it hears back.
                ingress.command(IngressCommand::ProbeApplied { logical_peer, ring });
            }
            SrtIngressEvent::Disconnected {
                peer,
                logical_peer,
                reason,
            } => {
                if let Some(Session::Publish(publisher)) = sessions.remove(&logical_peer) {
                    self.finish_publisher(publisher).await;
                }
                info!(peer = %peer, %reason, "SRT peer disconnected");
            }
        }
    }

    async fn handle_connected(
        &self,
        ingress: &mut Ingress,
        sessions: &mut HashMap<IngressPeer, Session>,
        peer: SocketAddr,
        logical_peer: IngressPeer,
        stream_id: String,
    ) {
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
                    Ok((session, media)) => {
                        sessions.insert(logical_peer, Session::Publish(session));
                        ingress.command(IngressCommand::AttachPublisher {
                            logical_peer,
                            media,
                        });
                    }
                    Err(error) => {
                        warn!(peer = %peer, %error, "rejecting SRT publisher");
                        ingress.disconnect(logical_peer);
                    }
                }
            }
            SrtConnectionMode::Read => match self.start_reader(&pipeline.id).await {
                Ok(reader) => {
                    sessions.insert(
                        logical_peer,
                        Session::Read {
                            pipeline_id: pipeline.id,
                            closing: false,
                        },
                    );
                    ingress.command(IngressCommand::AttachReader {
                        logical_peer,
                        reader,
                    });
                }
                Err(error) => {
                    warn!(peer = %peer, %error, "rejecting SRT reader");
                    ingress.disconnect(logical_peer);
                }
            },
        }
    }

    /// Register an admitted publisher and build the media state the Owner
    /// will run. Returns Tokio's bookkeeping half and the Owner's media half.
    pub(super) async fn start_publisher(
        &self,
        peer: SocketAddr,
        pipeline: crate::media::ingest_auth::AuthenticatedPipeline,
        stream_key: String,
    ) -> Result<(PublisherSession, Box<SrtPublisherMedia>), String> {
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
        let media = Box::new(SrtPublisherMedia {
            registration: registration.clone(),
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
        });
        let session = PublisherSession {
            pipeline_id: pipeline.id,
            registration,
            closing: false,
            quality_fold: QualityFold::default(),
        };
        Ok((session, media))
    }

    /// Apply a publisher's first stream probe to its ingest session, and adapt
    /// the pipeline ring when this input is selected. Returns the replacement
    /// ring for the Owner.
    pub(super) async fn apply_probe(
        &self,
        publisher: &PublisherSession,
        probe: DemuxProbe,
    ) -> Option<Arc<RingBuffer>> {
        let video_fps = probe.video.as_ref().map(|video| video.fps).unwrap_or(30.0);
        let audio_track_count = probe.audio_tracks.len();
        let first_audio = probe.audio_tracks.first().cloned();
        let selected_video_track_index = probe.video.as_ref().map(|_| 0);
        self.engine
            .update_ingest_session_meta(
                &publisher.pipeline_id,
                &publisher.registration,
                probe.video,
                first_audio,
                None,
            )
            .await;
        self.engine
            .update_ingest_session_video_track_selection(
                &publisher.registration,
                probe.video_track_count,
                selected_video_track_index,
            )
            .await;
        if !probe.audio_tracks.is_empty() {
            self.engine
                .update_ingest_session_audio_tracks(
                    &publisher.pipeline_id,
                    &publisher.registration,
                    probe.audio_tracks,
                )
                .await;
        }
        if self
            .engine
            .is_ingest_session_selected(&publisher.pipeline_id, &publisher.registration)
            .await
        {
            self.engine
                .adapt_pipeline_ring(&publisher.pipeline_id, video_fps, audio_track_count)
                .await
        } else {
            None
        }
    }

    async fn start_reader(&self, pipeline_id: &str) -> Result<SrtReaderMedia, String> {
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
        Ok(SrtReaderMedia::new(TsChunkReader::new(
            format!("srt_play:{pipeline_id}"),
            &muxer,
        )))
    }

    /// Ingest bookkeeping after the Owner has flushed the publisher's media.
    pub(super) async fn finish_publisher(&self, publisher: PublisherSession) {
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

/// Close sessions whose pipeline was deleted: a sink pipeline can disappear
/// while its SRT publisher is healthy, and a reader's target independently of
/// its socket. The pipeline ring is removed by deletion immediately, while the
/// active ingest entry intentionally remains until peer teardown completes, so
/// ring ownership is the lifecycle signal. Asking the Owner to disconnect the
/// real peer lets the remote side observe a normal shutdown promptly.
async fn close_deleted_sessions(
    engine: &MediaEngine,
    ingress: &mut Ingress,
    sessions: &mut HashMap<IngressPeer, Session>,
) {
    let live_pipelines = engine.ingests.pipelines.read().await;
    let mut to_close = Vec::new();
    for (peer, session) in sessions.iter_mut() {
        let (pipeline_id, closing) = match session {
            Session::Publish(publisher) => (&publisher.pipeline_id, &mut publisher.closing),
            Session::Read {
                pipeline_id,
                closing,
            } => (&*pipeline_id, closing),
        };
        if !*closing && !live_pipelines.contains_key(pipeline_id) {
            *closing = true;
            to_close.push(*peer);
        }
    }
    drop(live_pipelines);
    for peer in to_close {
        ingress.disconnect(peer);
    }
}
