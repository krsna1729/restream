//! MoQ ingest (Q-070): a publisher pushes a live stream to Restream over
//! QUIC or WebTransport (moq-lite + hang) and it feeds a keyed pipeline
//! input, as an RTMP, SRT or HLS push does.
//!
//! Off unless `RESTREAM_MOQ_INGEST_LISTEN` is set ([`crate::config::MoqIngestSetting`]).
//! Ownership follows SRT ingest: the endpoint, every connection, frame
//! conversion and the ring push run on the "moq-ingest" thread with its own
//! Compio runtime ([`listener`], [`session`], [`media`]); Tokio owns
//! lifecycle only. This module holds the Tokio half ([`MoqIngestServer`]:
//! authentication, input registration as protocol `"moq"`, the stream
//! probe, disconnect bookkeeping) and the bridge the listener awaits it
//! through ([`Lifecycle`], [`TokioLifecycle`]).

use std::net::SocketAddr;
use std::sync::Arc;

use tracing::warn;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::ingest_auth::{PipelineAccessAuthenticator, PipelineAccessMode};
use crate::media::ring_buffer::RingBuffer;
use crate::media::security::{IngestSecurityService, RateLimitScope};

pub(crate) mod cmaf;
pub(crate) mod listener;
pub(crate) mod media;
pub(crate) mod publisher;
pub(crate) mod session;

use media::MoqProbe;
use publisher::{ProbeHoldLimits, PublisherCounters, PublisherMedia};

/// The input registration's protocol label (runtime state, events, UI).
pub(crate) const MOQ_PROTOCOL: &str = "moq";

/// Longest stream key a publisher may name; a longer PATH is refused before
/// any lookup.
pub(crate) const MAX_STREAM_KEY_BYTES: usize = 512;

/// What Tokio keeps about an admitted publisher: identity, never media.
#[derive(Clone)]
pub(crate) struct PublisherSession {
    pub(crate) pipeline_id: String,
    pub(crate) registration: IngestRegistration,
}

/// An admitted publisher: Tokio's bookkeeping half and the media half the
/// Owner runs.
pub(crate) struct Admission {
    pub(crate) session: PublisherSession,
    pub(crate) media: PublisherMedia,
}

/// Why a publisher was not admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rejection {
    /// The client's address is banned under `moq-publish`.
    Banned,
    /// No enabled keyed input has this stream key.
    Unauthorized,
    /// The input already has a publisher.
    Duplicate,
    /// Lookup or engine failure; the publisher may retry.
    Unavailable(String),
}

impl Rejection {
    pub(crate) fn reason(&self) -> &str {
        match self {
            Self::Banned => "client is temporarily banned",
            Self::Unauthorized => "unknown or disabled stream key",
            Self::Duplicate => "the input already has a publisher",
            Self::Unavailable(reason) => reason,
        }
    }
}

/// The stream key an announced broadcast path names: its last segment
/// (query dropped), without moq.dev's `.hang` suffix. The full path stays
/// with the profile (relays scope sessions and tokens by it). `None` when
/// empty or longer than [`MAX_STREAM_KEY_BYTES`].
pub(crate) fn stream_key_from_path(path: &str) -> Option<&str> {
    let path = path.split_once('?').map_or(path, |(path, _query)| path);
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let key = last.strip_suffix(".hang").unwrap_or(last);
    (!key.is_empty() && key.len() <= MAX_STREAM_KEY_BYTES).then_some(key)
}

/// Tokio's half of MoQ ingest: the same engine calls SRT ingest makes, for
/// protocol `"moq"`.
pub(crate) struct MoqIngestServer {
    pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
    engine: Arc<MediaEngine>,
    security: Arc<IngestSecurityService>,
    probe_hold: ProbeHoldLimits,
}

impl MoqIngestServer {
    pub(crate) fn new(
        pipeline_access: Arc<dyn PipelineAccessAuthenticator>,
        engine: Arc<MediaEngine>,
        security: Arc<IngestSecurityService>,
    ) -> Self {
        Self {
            pipeline_access,
            engine,
            security,
            probe_hold: ProbeHoldLimits::default(),
        }
    }

    pub(crate) fn engine(&self) -> &Arc<MediaEngine> {
        &self.engine
    }

    /// Authenticate `stream_key` for `peer` under `moq-publish`, register
    /// the input attempt and build the publisher's media state.
    pub(crate) async fn admit(
        &self,
        peer: SocketAddr,
        stream_key: &str,
    ) -> Result<Admission, Rejection> {
        let client_ip = peer.ip().to_string();
        if self
            .security
            .is_ip_banned_for(RateLimitScope::MoqPublish, &client_ip)
            .is_some()
        {
            return Err(Rejection::Banned);
        }
        let pipeline = self
            .pipeline_access
            .authenticate(PipelineAccessMode::MoqPublish, stream_key, &client_ip)
            .await
            .map_err(|error| match error {
                crate::media::ingest_auth::PipelineAccessError::InvalidStreamKey => {
                    Rejection::Unauthorized
                }
                crate::media::ingest_auth::PipelineAccessError::LookupFailed(error) => {
                    Rejection::Unavailable(format!("stream key lookup failed: {error}"))
                }
            })?;
        let ring = self.engine.get_or_create_pipeline(&pipeline.id).await;
        let registration = self
            .engine
            .try_register_pipeline_input_attempt(
                &pipeline.id,
                &pipeline.input_id,
                stream_key,
                MOQ_PROTOCOL,
                pipeline.selected,
            )
            .await
            .ok_or(Rejection::Duplicate)?;
        self.engine
            .update_ingest_session_meta(
                &pipeline.id,
                &registration,
                None,
                None,
                Some(peer.to_string()),
            )
            .await;
        let session = PublisherSession {
            pipeline_id: pipeline.id,
            registration,
        };
        let Some(counters) = self
            .engine
            .with_ingest_session(&session.registration, |ingest| PublisherCounters {
                bytes_received: ingest.bytes_received.clone(),
                first_media_ms: ingest.first_media_ms.clone(),
                ingest_metrics: ingest.metrics.clone(),
                last_progress_ms: ingest.last_progress_ms.clone(),
                keyframe_times: ingest.keyframe_times.clone(),
            })
            .await
        else {
            self.engine
                .unregister_ingest_if_current(&session.pipeline_id, &session.registration)
                .await;
            return Err(Rejection::Unavailable(
                "the ingest session disappeared during registration".to_string(),
            ));
        };
        let media = PublisherMedia::new(
            session.registration.clone(),
            ring,
            counters,
            self.probe_hold,
        );
        Ok(Admission { session, media })
    }

    /// Record a publisher's stream probe and adapt the pipeline ring when
    /// this input is selected; returns the replacement ring. As in SRT
    /// ingest, the video/audio metadata goes last: it opens an output's
    /// start gate, so the output attaches to the resized ring.
    pub(crate) async fn apply_probe(
        &self,
        session: &PublisherSession,
        probe: MoqProbe,
    ) -> Option<Arc<RingBuffer>> {
        let video_fps = probe.video.as_ref().map_or(30.0, |video| video.fps);
        let audio_track_count = probe.audio_tracks.len();
        let first_audio = probe.audio_tracks.first().cloned();
        self.engine
            .update_ingest_session_video_track_selection(
                &session.registration,
                usize::from(probe.video.is_some()),
                probe.video.as_ref().map(|_| 0),
            )
            .await;
        if !probe.audio_tracks.is_empty() {
            self.engine
                .update_ingest_session_audio_tracks(
                    &session.pipeline_id,
                    &session.registration,
                    probe.audio_tracks,
                )
                .await;
        }
        let ring = if self
            .engine
            .is_ingest_session_selected(&session.pipeline_id, &session.registration)
            .await
        {
            self.engine
                .adapt_pipeline_ring(&session.pipeline_id, video_fps, audio_track_count)
                .await
        } else {
            None
        };
        self.engine
            .update_ingest_session_meta(
                &session.pipeline_id,
                &session.registration,
                probe.video,
                first_audio,
                None,
            )
            .await;
        self.engine
            .note_ingest_session_probed(&session.registration)
            .await;
        ring
    }

    /// Ingest bookkeeping after the Owner flushed the publisher's media.
    pub(crate) async fn finish(&self, session: PublisherSession, reason: String) {
        self.engine
            .record_ingest_disconnect_if_current(
                &session.pipeline_id,
                &session.registration,
                Some("disconnect"),
                Some(reason),
                false,
            )
            .await;
        self.engine
            .unregister_ingest_if_current(&session.pipeline_id, &session.registration)
            .await;
    }
}

/// What a connection on the Owner thread asks of Tokio. Generic so the
/// session runs against a double in tests.
pub(crate) trait Lifecycle {
    async fn admit(&self, peer: SocketAddr, stream_key: &str) -> Result<Admission, Rejection>;
    async fn apply_probe(
        &self,
        session: &PublisherSession,
        probe: MoqProbe,
    ) -> Option<Arc<RingBuffer>>;
    /// Fire and forget: the media is already flushed.
    fn finish(&self, session: PublisherSession, reason: String);
}

/// The production bridge: each request runs as a Tokio task and answers
/// over a oneshot the Owner awaits. Nothing on the Owner thread blocks.
#[derive(Clone)]
pub(crate) struct TokioLifecycle {
    runtime: tokio::runtime::Handle,
    server: Arc<MoqIngestServer>,
}

impl TokioLifecycle {
    pub(crate) fn new(runtime: tokio::runtime::Handle, server: Arc<MoqIngestServer>) -> Self {
        Self { runtime, server }
    }
}

impl Lifecycle for TokioLifecycle {
    async fn admit(&self, peer: SocketAddr, stream_key: &str) -> Result<Admission, Rejection> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let server = self.server.clone();
        let stream_key = stream_key.to_string();
        self.runtime.spawn(async move {
            let result = server.admit(peer, &stream_key).await;
            deliver_admission(&server, answer, result).await;
        });
        answered
            .await
            .unwrap_or_else(|_| Err(Rejection::Unavailable("admission was abandoned".into())))
    }

    async fn apply_probe(
        &self,
        session: &PublisherSession,
        probe: MoqProbe,
    ) -> Option<Arc<RingBuffer>> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let server = self.server.clone();
        let session = session.clone();
        self.runtime.spawn(async move {
            let _ = answer.send(server.apply_probe(&session, probe).await);
        });
        answered.await.ok().flatten()
    }

    fn finish(&self, session: PublisherSession, reason: String) {
        let server = self.server.clone();
        self.runtime
            .spawn(async move { server.finish(session, reason).await });
    }
}

/// Hand an admission to the connection that asked for it. A connection that
/// went away meanwhile cannot take it, so the registration is released here
/// rather than left holding the input.
async fn deliver_admission(
    server: &MoqIngestServer,
    answer: tokio::sync::oneshot::Sender<Result<Admission, Rejection>>,
    result: Result<Admission, Rejection>,
) {
    if let Err(Ok(admission)) = answer.send(result) {
        warn!(
            pipeline_id = %admission.session.pipeline_id,
            "MoQ publisher left during admission; releasing its input"
        );
        server
            .finish(
                admission.session,
                "MoQ publisher left during admission".to_string(),
            )
            .await;
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod lifecycle_tests;
