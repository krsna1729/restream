//! Tokio session lifecycle for srt-rs ingress: admission, publisher and reader
//! registration, stream probes/topology updates, disconnect bookkeeping, receive
//! quality and deletion checks. Media never passes through here: it runs to
//! completion on the ingress Owner thread (`ingress_media`).
//!
//! The loop shared by every peer awaits no engine lock. Admission (a
//! listener peer's lookup and registration, a call's registration) runs as
//! one task per connecting peer (`admission`); an attached publisher's probe
//! applications and its finish run in that publisher's task, in event order
//! (`peer_work`). Telemetry writes the publisher's own ingest state, and the
//! deleted-pipeline check skips a tick while the ring map is being written.

mod admission;
mod peer_work;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tracing::{error, info, warn};

use crate::engine::{ActiveIngest, MediaEngine};
use crate::ingest_auth::PipelineAccessAuthenticator;
use crate::ingest_lease::{IngestEnd, IngestLease};
use crate::input_gate::InputTimestampMapper;
use crate::security::IngestSecurityService;
use crate::ts_chunk_ring::TsChunkReader;
use restream_mpegts::DemuxProbe;
use restream_sync::RingBuffer;
use restream_sync::standby_gop::StandbyGopCache;

pub(crate) use super::srt_policy::SrtIngestPolicyStore;

use admission::Admissions;
#[cfg(test)]
pub(super) use admission::{AUTH_DEADLINE, MAX_ADMISSIONS_PER_KEY};
use peer_work::{PeerWork, ProbeAnswer, ProbeJob};

use super::calls::CallSupervisor;
use super::ingress_admission::{AUTH_FAILURE_CAPACITY, AdmissionGuard, ReceiverGroupId};
use super::ingress_media::{SrtPublisherMedia, SrtReaderMedia};
use super::ingress_owner::{
    INGRESS_COMMAND_CAPACITY, INGRESS_EVENT_CAPACITY, INGRESS_TELEMETRY_CAPACITY, IngressCommand,
    IngressConfig, IngressPeer, SrtIngressEvent, SrtIngressHandle,
};
use super::ingress_quality::{QualityFold, QualitySample};

/// How often deleted pipelines are checked and the shutdown flag is read.
/// Lifecycle only: media does not wait on it.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(100);

/// One command may wait for an Owner bridge. While it does, event consumption
/// is backpressured: neither another Owner nor PMT churn can grow a reply queue.
/// Housekeeping/telemetry remain responsive on the existing listener select.
struct Ingress {
    handle: SrtIngressHandle,
    pending_command: Option<IngressCommand>,
}

impl Ingress {
    fn command(&mut self, command: IngressCommand) -> Result<(), IngressCommand> {
        self.flush_commands();
        if self.pending_command.is_some() {
            return Err(command);
        }
        self.pending_command = Some(command);
        self.flush_commands();
        Ok(())
    }

    fn disconnect(&mut self, peer: IngressPeer) -> Result<(), IngressCommand> {
        self.command(IngressCommand::Disconnect { logical_peer: peer })
    }

    fn flush_commands(&mut self) {
        if let Some(command) = self.pending_command.take() {
            self.pending_command = self.handle.try_send(command).err();
        }
    }

    async fn next_event(
        events: &mut tokio::sync::mpsc::Receiver<SrtIngressEvent>,
        pending: &Option<IngressCommand>,
    ) -> Option<SrtIngressEvent> {
        if pending.is_some() {
            std::future::pending().await
        } else {
            events.recv().await
        }
    }
}

pub struct SrtServer {
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    engine: Arc<MediaEngine>,
    security: Arc<IngestSecurityService>,
    ingest_policy_store: Arc<SrtIngestPolicyStore>,
}

/// What Tokio keeps about a session: identity for bookkeeping, never media.
enum Session {
    Publish(Publisher),
    Read { pipeline_id: String, closing: bool },
}

/// A registered publisher attempt, from its admission to its finish.
pub(super) struct PublisherSession {
    /// Dropped with the session (a panicking task, a listener torn down), it
    /// releases the input attempt.
    lease: IngestLease,
    /// The attempt's own ingest state: written without a registry lookup.
    ingest: Arc<ActiveIngest>,
}

/// What the loop keeps about an attached publisher; its lease lives in its
/// task (`peer_work`), which finishes it once this is dropped.
struct Publisher {
    /// The publisher task's probe queue.
    jobs: tokio::sync::mpsc::UnboundedSender<ProbeJob>,
    ingest: Arc<ActiveIngest>,
    /// The attempt's own token: cancelled on disconnect, it frees the input
    /// for a reconnect at once, before the task's finish runs.
    cancel: tokio_util::sync::CancellationToken,
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
        // The guard lives as long as this loop, so the failure channel never
        // disconnects under the select arm below.
        let (auth_failures, auth_failure_rx) = flume::bounded(AUTH_FAILURE_CAPACITY);
        let admission_guard = AdmissionGuard {
            security: self.security.clone(),
            failures: auth_failures,
            stats: self.engine.listener_stats_handle(),
        };
        let handle = match SrtIngressHandle::start(IngressConfig {
            bind,
            policy_store: self.ingest_policy_store.clone(),
            receiver_group: ReceiverGroupId::generate(),
            admission_guard: admission_guard.clone(),
            stats: self.engine.listener_stats_handle(),
            command_capacity: INGRESS_COMMAND_CAPACITY,
            event_capacity: INGRESS_EVENT_CAPACITY,
            telemetry_capacity: INGRESS_TELEMETRY_CAPACITY,
            owners: self.engine.config.srt_ingress_owners,
            max_peers_per_ip: self.engine.config.srt_max_peers_per_ip,
            window_memory_bytes: self.engine.config.srt_ingress_window_memory_bytes,
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
            pending_command: None,
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
        let mut calls: CallSupervisor = CallSupervisor::new();
        let mut admissions = Admissions::new(self.engine.config.srt_max_peers_per_ip);
        let (mut peers, mut answers) = PeerWork::new();

        info!(port, bind = %ingress.handle.local_addr(), "SRT listener ready (srt-rs compio Owner ingress)");
        let mut owner_fault = None;
        let mut housekeeping = tokio::time::interval(HOUSEKEEPING_INTERVAL);
        housekeeping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                event = Ingress::next_event(&mut ingress.handle.events, &ingress.pending_command) => match event {
                    Some(SrtIngressEvent::Fault { detail }) => {
                        owner_fault = Some(detail);
                        break;
                    }
                    Some(event) => {
                        if self.handle_ingress_event(&mut ingress, &mut sessions, &mut calls, &mut admissions, event).is_err() {
                            owner_fault = Some("SRT ingress command reservation exhausted".to_string());
                            break;
                        }
                    }
                    None => {
                        owner_fault = Some("the SRT ingress owner thread stopped".to_string());
                        break;
                    }
                },
                Some(resolved) = calls.resolved.recv(), if ingress.pending_command.is_none() => {
                    if let Some(command) = calls.resolved(resolved, std::time::Instant::now()) {
                        // The slot is free (the guard above), so this is accepted.
                        let _ = ingress.command(command);
                    }
                }
                // Completions are backpressured like events: applying one may
                // issue a command, so it runs only with the slot free.
                Some(joined) = admissions.join_next(), if ingress.pending_command.is_none() => {
                    if self.apply_admission(&mut ingress, &mut sessions, &mut peers, joined).is_err() {
                        owner_fault = Some("SRT ingress command reservation exhausted".to_string());
                        break;
                    }
                }
                // A probe answer is one command: backpressured the same way.
                Some(ProbeAnswer { logical_peer, identity, ring }) = answers.recv(), if ingress.pending_command.is_none() => {
                    // The slot is free (the guard above), so this is accepted.
                    let _ = ingress.command(IngressCommand::ProbeApplied { logical_peer, identity, ring });
                }
                Some(()) = peers.reap() => {}
                Ok((scope, ip)) = auth_failure_rx.recv_async() => {
                    if self.security.record_failure_for(scope, &ip.to_string()) {
                        warn!(peer_ip = %ip, scope = scope.key(), "SRT caller banned after repeated unknown stream keys");
                    }
                }
                sample = ingress.handle.telemetry.recv() => {
                    if let Some(sample) = sample {
                        fold_telemetry(&mut sessions, sample);
                    }
                }
                _ = housekeeping.tick() => {
                    if shutdown.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }
                    close_deleted_sessions(&self.engine, &mut ingress, &mut sessions);
                    calls.tick(&self.engine.ingests.srt_calls, std::time::Instant::now(), |peer| {
                        ingress.pending_command.is_none() && ingress.disconnect(peer).is_ok()
                    });
                    if ingress.handle.is_closed() {
                        owner_fault = Some("the SRT ingress owner thread stopped".to_string());
                        break;
                    }
                }
            }
            ingress.flush_commands();
        }
        // Held to here so the failure channel stayed open for the select.
        drop(admission_guard);

        // Deliver the one already-accepted command before shutdown. Native
        // Owner shutdown closes all peers and flushes their last media.
        let exit = ingress.handle.shutdown(ingress.pending_command).await;
        if let Some(fault) = owner_fault.as_ref().or(exit.fault.as_ref()) {
            error!(port, fault = %fault, quiescent = exit.quiescent, "SRT listener stopped after an owner fault");
        } else if !exit.quiescent {
            warn!(
                port,
                "SRT ingress owner did not reach quiescence at shutdown"
            );
        }
        self.drain_admissions(&mut admissions, &mut peers).await;
        // Dropping each publisher's handle closes its queue: its task applies
        // what is queued, then finishes the attempt.
        sessions.clear();
        peers.drain().await;
        self.engine
            .listener_stats_handle()
            .bonding_available
            .store(false, std::sync::atomic::Ordering::Relaxed);
        info!(port, quiescent = exit.quiescent, "SRT listener stopped");
    }

    fn handle_ingress_event(
        self: &Arc<Self>,
        ingress: &mut Ingress,
        sessions: &mut HashMap<IngressPeer, Session>,
        calls: &mut CallSupervisor,
        admissions: &mut Admissions,
        event: SrtIngressEvent,
    ) -> Result<(), IngressCommand> {
        match event {
            SrtIngressEvent::Fault { .. } => {}
            SrtIngressEvent::Connected {
                peer,
                logical_peer,
                stream_id,
            } => {
                if calls.owns(&logical_peer) {
                    if let Some(caller) = calls.connected(logical_peer) {
                        self.start_call(admissions, peer, logical_peer, caller);
                    }
                } else {
                    self.start_admission(ingress, admissions, peer, logical_peer, stream_id)?;
                }
            }
            SrtIngressEvent::CallStarted {
                token,
                logical_peer,
            } => {
                if !calls.started(token, logical_peer) {
                    ingress.disconnect(logical_peer)?;
                }
            }
            SrtIngressEvent::CallFailed { token, reason } => {
                calls.failed(token, &reason, std::time::Instant::now());
            }
            SrtIngressEvent::Probe {
                logical_peer,
                identity,
                probe,
            } => {
                // Always answered: the Owner holds the publisher's packets
                // until it hears back. A publisher's task answers its own
                // probes, in order; anything else is answered here.
                let unqueued = match sessions.get(&logical_peer) {
                    Some(Session::Publish(publisher)) => {
                        publisher.jobs.send(ProbeJob { identity, probe }).is_err()
                    }
                    _ => true,
                };
                if unqueued {
                    ingress.command(IngressCommand::ProbeApplied {
                        logical_peer,
                        identity,
                        ring: None,
                    })?;
                }
            }
            SrtIngressEvent::Disconnected {
                peer,
                logical_peer,
                reason,
            } => {
                // The input is free at once, as a dropped lease frees it: a
                // reconnect must not be refused while this peer's task still
                // waits. Dropping the publisher closes the task's queue: the
                // task answers the probes queued before this event (with no
                // ring: the attempt is cancelled), then finishes.
                if let Some(Session::Publish(publisher)) = sessions.remove(&logical_peer) {
                    publisher.cancel.cancel();
                }
                admissions.abandon(&logical_peer);
                calls.disconnected(logical_peer, &reason, std::time::Instant::now());
                info!(peer = %peer, %reason, "SRT peer disconnected");
            }
        }
        Ok(())
    }

    /// Register an admitted publisher and build the media state the Owner
    /// will run. Returns Tokio's bookkeeping half and the Owner's media half.
    pub(super) async fn start_publisher(
        &self,
        peer: SocketAddr,
        pipeline: crate::ingest_auth::AuthenticatedPipeline,
        stream_key: String,
    ) -> Result<(PublisherSession, Box<SrtPublisherMedia>), String> {
        let ring_buffer = self.engine.get_or_create_pipeline(&pipeline.id).await;
        // Every early return and unwind below drops the lease, which
        // releases the attempt.
        let lease = self
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
        #[cfg(test)]
        injected_panics::unwind_if_marked(&stream_key);
        self.engine
            .update_ingest_session_meta(
                &pipeline.id,
                lease.registration(),
                None,
                None,
                Some(peer.to_string()),
            )
            .await;
        let Some(ingest) = self
            .engine
            .current_ingest_session(lease.registration())
            .await
        else {
            return Err("SRT ingest session disappeared during registration".to_string());
        };
        let media = Box::new(SrtPublisherMedia {
            registration: lease.registration().clone(),
            ring_buffer,
            demuxer: restream_mpegts::TsDemuxer::new(),
            timestamp_mapper: InputTimestampMapper::default(),
            standby_gop: StandbyGopCache::default(),
            packets: Vec::with_capacity(16),
            keyframe_times: ingest.keyframe_times.clone(),
            bytes_received: ingest.bytes_received.clone(),
            first_media_ms: ingest.first_media_ms.clone(),
            ingest_metrics: ingest.metrics.clone(),
            last_progress_ms: ingest.last_progress_ms.clone(),
        });
        Ok((PublisherSession { lease, ingest }, media))
    }

    /// Apply an authoritative container snapshot and adapt the selected ring.
    /// The engine serializes the attempt/selection check and complete metadata
    /// replacement with the pipeline's selection and origin resolution, and
    /// reports the probe recorded. Resize precedes the metadata that opens
    /// the output start gate.
    pub(super) async fn apply_probe(
        &self,
        publisher: &PublisherSession,
        identity: super::ingress_media::ProbeIdentity,
        probe: DemuxProbe,
    ) -> Option<Arc<RingBuffer>> {
        let registration = publisher.lease.registration();
        if registration.attempt_id != identity.attempt_id {
            return None;
        }
        self.engine
            .replace_ingest_session_tracks(
                &publisher.ingest,
                registration,
                probe.video,
                probe.audio_tracks,
                probe.video_track_count,
                probe.video_sequence_header,
            )
            .await
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
        let (muxer, lease) = self
            .engine
            .get_or_create_ts_muxer_stage(pipeline_id, "play", ring)
            .await;
        Ok(SrtReaderMedia::new(
            TsChunkReader::new(format!("srt_play:{pipeline_id}"), &muxer),
            lease,
        ))
    }

    /// Ingest bookkeeping after the Owner has flushed the publisher's media.
    pub(super) async fn finish_publisher(&self, publisher: PublisherSession) {
        publisher
            .lease
            .finish(IngestEnd::Disconnected {
                phase: "disconnect",
                reason: "SRT peer disconnected".to_string(),
                had_error: false,
            })
            .await;
    }
}

/// Fold one Owner-stamped receive-quality sample into its publisher's own
/// ingest state, under that state's short metadata lock only: a superseded
/// attempt writes state no registry serves any more. A sample for a peer
/// with no publisher session is dropped; the next one (about a second later)
/// replaces it.
fn fold_telemetry(
    sessions: &mut HashMap<IngressPeer, Session>,
    QualitySample { peer, observation }: QualitySample,
) {
    let Some(Session::Publish(publisher)) = sessions.get_mut(&peer) else {
        return;
    };
    if let Some(quality) = publisher.quality_fold.fold(observation) {
        restream_sync::lock::write(&publisher.ingest.metadata).quality = quality;
    }
}

/// A lifecycle task (`admission`, `publisher`) ended without its result.
fn report_failed(task: &'static str, error: tokio::task::JoinError) {
    if error.is_panic() {
        let payload = error.into_panic();
        let message = crate::panic_boundary::record_contained(&*payload);
        error!(panic = message, task, "SRT lifecycle task panicked");
    } else {
        error!(%error, task, "SRT lifecycle task was cancelled");
    }
}

/// Close sessions whose pipeline was deleted: a sink pipeline can disappear
/// while its SRT publisher is healthy, and a reader's target independently of
/// its socket. The pipeline ring is removed by deletion immediately, while the
/// active ingest entry intentionally remains until peer teardown completes, so
/// ring ownership is the lifecycle signal. Asking the Owner to disconnect the
/// real peer lets the remote side observe a normal shutdown promptly. A ring
/// map being written (a ring created or resized) skips this tick rather than
/// wait on the loop; the next is [`HOUSEKEEPING_INTERVAL`] away.
fn close_deleted_sessions(
    engine: &MediaEngine,
    ingress: &mut Ingress,
    sessions: &mut HashMap<IngressPeer, Session>,
) {
    if ingress.pending_command.is_some() {
        return;
    }
    let Ok(live_pipelines) = engine.ingests.pipelines.try_read() else {
        return;
    };
    for (peer, session) in sessions.iter_mut() {
        let (pipeline_id, closing) = match session {
            Session::Publish(publisher) => (
                publisher.ingest.pipeline_id.as_str(),
                &mut publisher.closing,
            ),
            Session::Read {
                pipeline_id,
                closing,
            } => (pipeline_id.as_str(), closing),
        };
        if !*closing && !live_pipelines.contains_key(pipeline_id) {
            if ingress.disconnect(*peer).is_ok() {
                *closing = true;
            }
            if ingress.pending_command.is_some() {
                break;
            }
        }
    }
}

/// Test-only fault point: `start_publisher` for a marked stream key unwinds
/// right after it registered the input attempt, as a bug there would.
/// `resume_unwind` skips the panic hook, so logs stay quiet.
#[cfg(test)]
pub(super) mod injected_panics {
    use std::collections::HashSet;
    use std::sync::Mutex;

    static MARKED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

    pub(in crate::srt) fn mark(stream_key: &str) {
        restream_sync::lock::lock(&MARKED)
            .get_or_insert_with(HashSet::new)
            .insert(stream_key.to_string());
    }

    pub(super) fn unwind_if_marked(stream_key: &str) {
        let marked = restream_sync::lock::lock(&MARKED)
            .as_mut()
            .is_some_and(|marked| marked.remove(stream_key));
        if marked {
            std::panic::resume_unwind(Box::new("injected SRT start_publisher panic"));
        }
    }
}

#[cfg(test)]
mod tests;
