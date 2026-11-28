//! The Tokio-side control session of one RTMP connection: publish and play
//! authorization, playback bursts, ingest metadata, and the finish.

use super::*;
use crate::ingest_auth::AuthenticatedPipeline;

/// The publisher's disconnect as reported with Finish: (phase, reason,
/// had_error).
type Disconnect = (String, String, bool);

/// What the session does after one command.
enum Step {
    Continue,
    /// Shutdown, a cancelled publish authorization, or the connection
    /// closing its command channel.
    Stop,
    Finish(Disconnect, oneshot::Sender<()>),
}

pub(in crate::rtmp) async fn run_rtmp_control_session(
    commands: mpsc::Receiver<RtmpControlCommand>,
    shutdown: CancellationToken,
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
) {
    let mut session = ControlSession {
        commands,
        shutdown,
        pipeline_access,
        security,
        engine,
        active_ingest: None,
        playback: None,
        deferred_command: None,
    };
    let (disconnect, finish_reply) = loop {
        let Some(command) = session.next_command().await else {
            break (None, None);
        };
        // Play bursts, the per-burst command, skip the general handler.
        let step = match command {
            RtmpControlCommand::PlayNext { reply, recycled } => {
                session.play_next(reply, recycled).await;
                Step::Continue
            }
            command => session.handle(command).await,
        };
        match step {
            Step::Continue => {}
            Step::Stop => break (None, None),
            Step::Finish(disconnect, reply) => break (Some(disconnect), Some(reply)),
        }
    };
    session.finish_ingest(disconnect).await;
    if let Some(reply) = finish_reply {
        let _ = reply.send(());
    }
}

/// An RTMP play reader and its reusable burst buffer.
struct Playback {
    reader: Reader,
    burst: Vec<Arc<MediaPacket>>,
}

struct ControlSession {
    commands: mpsc::Receiver<RtmpControlCommand>,
    shutdown: CancellationToken,
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    engine: Arc<MediaEngine>,
    /// The publisher's lease: dropped with this task, it releases the input
    /// attempt even when the task unwinds or is cancelled.
    active_ingest: Option<RegisteredIngest>,
    playback: Option<Playback>,
    /// A command that arrived while a play burst waited for media.
    deferred_command: Option<RtmpControlCommand>,
}

impl ControlSession {
    /// The deferred command, else the next one; `None` on shutdown or when
    /// the connection has closed its channel.
    async fn next_command(&mut self) -> Option<RtmpControlCommand> {
        if let Some(command) = self.deferred_command.take() {
            return Some(command);
        }
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => None,
            command = self.commands.recv() => command,
        }
    }

    async fn handle(&mut self, command: RtmpControlCommand) -> Step {
        match command {
            RtmpControlCommand::PublishRequested {
                stream_key,
                client_ip,
                client_addr,
                reply,
            } => {
                let decision = authorize_publish(
                    self.pipeline_access.as_ref(),
                    &self.security,
                    &self.engine,
                    &self.shutdown,
                    PublishContext {
                        client_ip: &client_ip,
                        client_addr: &client_addr,
                        stream_key: &stream_key,
                    },
                    &mut self.active_ingest,
                )
                .await;
                let cancelled = matches!(&decision, PublishAuthorization::Cancelled);
                let _ = reply.send(decision);
                if cancelled {
                    return Step::Stop;
                }
            }
            RtmpControlCommand::PublishAccepted { client_ip } => {
                self.security.record_success(&client_ip);
            }
            RtmpControlCommand::MediaProbe(event) => {
                if let Some(active) = self.active_ingest.as_ref() {
                    tokio::select! {
                        _ = self.shutdown.cancelled() => return Step::Stop,
                        _ = record_media_probe(&self.engine, active, event) => {}
                    }
                }
            }
            RtmpControlCommand::EncoderDeclared(declared) => {
                if let Some(active) = self.active_ingest.as_ref() {
                    self.engine
                        .update_ingest_session_declared(&active.registration, declared)
                        .await;
                }
            }
            RtmpControlCommand::PlayRequested {
                stream_key,
                client_ip,
                reply,
            } => {
                tokio::select! {
                    _ = self.shutdown.cancelled() => return Step::Stop,
                    authorization = authorize_play(
                        self.pipeline_access.as_ref(),
                        &self.engine,
                        &client_ip,
                        &stream_key,
                    ) => {
                        let _ = reply.send(authorization);
                    }
                }
            }
            RtmpControlCommand::PlayStart { pipeline_id, reply } => {
                return self.start_playback(pipeline_id, reply).await;
            }
            RtmpControlCommand::PlayNext { reply, recycled } => {
                self.play_next(reply, recycled).await
            }
            RtmpControlCommand::Cancel => self.playback = None,
            RtmpControlCommand::Quality(quality) => {
                if let Some(active) = self.active_ingest.as_ref() {
                    tokio::select! {
                        _ = self.shutdown.cancelled() => return Step::Stop,
                        _ = self.engine.update_ingest_session_quality(&active.registration, *quality) => {}
                    }
                }
            }
            RtmpControlCommand::Finish {
                phase,
                reason,
                had_error,
                reply,
            } => return Step::Finish((phase, reason, had_error), reply),
        }
        Step::Continue
    }

    /// Opens a play reader on the pipeline and replies with its sequence
    /// headers.
    async fn start_playback(
        &mut self,
        pipeline_id: String,
        reply: oneshot::Sender<(Option<Bytes>, Option<Bytes>)>,
    ) -> Step {
        let (headers, ring) = tokio::select! {
            _ = self.shutdown.cancelled() => return Step::Stop,
            state = async {
                let headers = self.engine.get_sequence_headers(&pipeline_id).await;
                let ring = self.engine.get_or_create_pipeline(&pipeline_id).await;
                (headers, ring)
            } => state,
        };
        self.playback = Some(Playback {
            reader: Reader::new("rtmp_play".to_string(), ring),
            burst: Vec::with_capacity(MEDIA_PULL_BURST_PACKETS),
        });
        let _ = reply.send(headers);
        Step::Continue
    }

    /// Replies with the next burst of play media, waiting for it; `None` (and
    /// playback ends) on shutdown, cancel, another command, or a closed
    /// channel.
    async fn play_next(
        &mut self,
        reply: oneshot::Sender<Option<Vec<Arc<MediaPacket>>>>,
        recycled: Vec<Arc<MediaPacket>>,
    ) {
        let Some(playback) = self.playback.as_mut() else {
            let _ = reply.send(None);
            return;
        };
        if playback.burst.capacity() == 0 {
            playback.burst = recycled;
        }
        playback.burst.clear();
        let cancelled = loop {
            match playback
                .reader
                .pull_burst(&mut playback.burst, MEDIA_PULL_BURST_PACKETS)
            {
                Ok(0) => {
                    tokio::select! {
                        _ = self.shutdown.cancelled() => break true,
                        _ = playback.reader.wait_for_data() => {}
                        command = self.commands.recv() => {
                            if let Some(command) = command
                                && !matches!(command, RtmpControlCommand::Cancel)
                            {
                                self.deferred_command = Some(command);
                            }
                            break true;
                        }
                    }
                }
                Err(_) => continue,
                Ok(_) => break false,
            }
        };
        // No per-burst log: at hundreds of players a burst-rate log
        // line is itself a material cost.
        let packets = if cancelled || self.commands.is_closed() {
            None
        } else {
            Some(std::mem::take(&mut playback.burst))
        };
        let finished = packets.is_none();
        let _ = reply.send(packets);
        if finished {
            self.playback = None;
        }
    }

    /// Ends the publisher's ingest with the reported disconnect, or with a
    /// shutdown or plain disconnect when none was reported.
    async fn finish_ingest(self, disconnect: Option<Disconnect>) {
        let Some(active) = self.active_ingest else {
            return;
        };
        let (phase, reason, had_error) = disconnect.unwrap_or_else(|| {
            let (phase, reason) = if self.shutdown.is_cancelled() {
                ("shutdown", "RTMP listener shutting down")
            } else {
                ("disconnect", "publisher disconnected")
            };
            (phase.to_string(), reason.to_string(), false)
        });
        info!(pipeline = %active.lease.pipeline_id(), "[rtmp] Publisher disconnected");
        active
            .lease
            .finish(IngestEnd::Disconnected {
                phase: &phase,
                reason,
                had_error,
            })
            .await;
    }
}

struct PublishContext<'a> {
    client_ip: &'a str,
    client_addr: &'a str,
    stream_key: &'a str,
}

const BAD_NAME: &str = "NetStream.Publish.BadName";

/// Ban check, stream key authentication and ingest registration; on
/// acceptance the lease moves into `active_ingest`.
async fn authorize_publish(
    pipeline_access: &dyn PipelineAccessAuthenticator,
    security: &IngestSecurityService,
    engine: &Arc<MediaEngine>,
    shutdown: &CancellationToken,
    context: PublishContext<'_>,
    active_ingest: &mut Option<RegisteredIngest>,
) -> PublishAuthorization {
    let registered = async {
        let pipeline =
            authenticate_publisher(pipeline_access, security, shutdown, &context).await?;
        register_publisher(engine, shutdown, &context, &pipeline).await
    };
    match registered.await {
        Ok((lease, media)) => {
            *active_ingest = Some(lease);
            PublishAuthorization::Accepted(media)
        }
        Err(refusal) => refusal,
    }
}

/// The pipeline the stream key names, unless the IP is banned or the key
/// is refused.
async fn authenticate_publisher(
    pipeline_access: &dyn PipelineAccessAuthenticator,
    security: &IngestSecurityService,
    shutdown: &CancellationToken,
    context: &PublishContext<'_>,
) -> Result<AuthenticatedPipeline, PublishAuthorization> {
    if security.is_ip_banned(context.client_ip).is_some() {
        return Err(PublishAuthorization::Rejected {
            code: BAD_NAME,
            description: "IP temporarily banned due to too many login/publish failures",
            error: "IP is banned",
        });
    }
    let auth_result = tokio::select! {
        biased;
        result = pipeline_access.authenticate(PipelineAccessMode::RtmpPublish, context.stream_key, context.client_ip) => result,
        _ = shutdown.cancelled() => return Err(PublishAuthorization::Cancelled),
    };
    auth_result.map_err(|error| publish_refusal(error, context.stream_key))
}

#[cold]
fn publish_refusal(error: PipelineAccessError, stream_key: &str) -> PublishAuthorization {
    match error {
        PipelineAccessError::InvalidStreamKey => {
            warn!(stream_key = %redact_secret(stream_key), "publish stream key not found");
            PublishAuthorization::Rejected {
                code: BAD_NAME,
                description: "Invalid stream key",
                error: "Invalid stream key",
            }
        }
        PipelineAccessError::InputUnreadable => {
            warn!(
                stream_key = %redact_secret(stream_key),
                "publish refused: the input's stored configuration is unreadable"
            );
            PublishAuthorization::Rejected {
                code: BAD_NAME,
                description: "Input unavailable",
                error: "Input configuration unreadable",
            }
        }
    }
}

/// Registers the input attempt and builds the publisher's media state. Every
/// early return drops the lease, which releases the attempt.
async fn register_publisher(
    engine: &Arc<MediaEngine>,
    shutdown: &CancellationToken,
    context: &PublishContext<'_>,
    pipeline: &AuthenticatedPipeline,
) -> Result<(RegisteredIngest, Box<RtmpPublisherMedia>), PublishAuthorization> {
    let lease = tokio::select! {
        biased;
        lease = engine.try_register_pipeline_input_attempt(
            &pipeline.id,
            &pipeline.input_id,
            context.stream_key,
            "rtmp",
            pipeline.selected,
        ) => lease,
        _ = shutdown.cancelled() => return Err(PublishAuthorization::Cancelled),
    };
    let lease = lease.ok_or(PublishAuthorization::Rejected {
        code: BAD_NAME,
        description: "Input already has an active publisher",
        error: "Input already has an active publisher",
    })?;
    if shutdown.is_cancelled() {
        return Err(PublishAuthorization::Cancelled);
    }
    let media = tokio::select! {
        media = publisher_media(engine, &lease.registration, &pipeline.id) => media?,
        _ = shutdown.cancelled() => return Err(PublishAuthorization::Cancelled),
    };
    tokio::select! {
        _ = engine.update_ingest_session_meta(
            &pipeline.id,
            &lease.registration,
            None,
            None,
            Some(context.client_addr.to_string()),
        ) => {}
        _ = shutdown.cancelled() => return Err(PublishAuthorization::Cancelled),
    };
    info!(pipeline = %pipeline.id, "[rtmp] Ingest registered");
    Ok((lease, media))
}

/// The pipeline ring and the registered ingest's shared counters, as the
/// publisher's media state.
async fn publisher_media(
    engine: &MediaEngine,
    registration: &crate::ingest_registry::IngestRegistration,
    pipeline_id: &str,
) -> Result<Box<RtmpPublisherMedia>, PublishAuthorization> {
    let ring = engine.get_or_create_pipeline(pipeline_id).await;
    let ingest = engine
        .with_ingest_session(registration, |ingest| {
            (
                ingest.bytes_received.clone(),
                ingest.first_media_ms.clone(),
                ingest.metrics.clone(),
                ingest.last_progress_ms.clone(),
                ingest.keyframe_times.clone(),
                ingest.video_sequence_header.clone(),
                ingest.audio_sequence_header.clone(),
            )
        })
        .await;
    let Some((
        bytes_received,
        first_media_ms,
        ingest_metrics,
        last_progress_ms,
        keyframe_times,
        video_sequence_header,
        audio_sequence_header,
    )) = ingest
    else {
        return Err(PublishAuthorization::Rejected {
            code: BAD_NAME,
            description: "Input already has an active publisher",
            error: "Active ingest disappeared during registration",
        });
    };
    Ok(Box::new(RtmpPublisherMedia {
        registration: registration.clone(),
        ring,
        bytes_received,
        first_media_ms,
        ingest_metrics,
        last_progress_ms,
        keyframe_times,
        video_sequence_header,
        audio_sequence_header,
        timestamp_mapper: InputTimestampMapper::default(),
        standby_gop: StandbyGopCache::default(),
        video_probed: false,
        audio_probed: false,
    }))
}
