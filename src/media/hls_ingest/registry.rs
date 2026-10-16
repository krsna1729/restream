//! Stream-key authorization, per-key upload slots, the global buffer budget
//! and the session registry. Runs on Tokio CONTROL; per object it does one
//! lookup and one bounded channel send, never media work.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tracing::{error, info, warn};

use super::ordering::SessionLimits;
use super::session::{self, HlsObject, SessionContext, SessionEnd};
use super::{BodyError, HlsIngestLimits, ObjectBody, ObjectKind, ObjectOutcome};
use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::executor::{MediaService, MediaServiceClass};
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessMode,
};
use crate::media::security::{IngestSecurityService, RateLimitScope};
use crate::secret_display::redact_secret;

/// Objects queued for one session; encoders upload sequentially, and the
/// per-key upload slots bound what can wait here anyway.
const OBJECT_QUEUE: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizeError {
    /// Unknown key, disabled input, or a banned client.
    Forbidden,
    LookupFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// Another publisher holds the input (RTMP, SRT, file).
    PublisherActive,
    /// The session ended under the object twice, or the MEDIA executor is
    /// unavailable.
    Unavailable,
}

struct SessionHandle {
    attempt_id: u64,
    objects: mpsc::Sender<HlsObject>,
}

pub struct HlsIngest {
    engine: Arc<MediaEngine>,
    access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    limits: HlsIngestLimits,
    budget: Arc<Semaphore>,
    /// Keyed by input id. Async so that two first objects for one key cannot
    /// both register an attempt; held across the start path only.
    sessions: tokio::sync::Mutex<HashMap<String, SessionHandle>>,
    upload_slots: std::sync::Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl HlsIngest {
    pub fn new(
        engine: Arc<MediaEngine>,
        access: Arc<dyn PipelineAccessAuthenticator>,
        security: Arc<IngestSecurityService>,
        limits: HlsIngestLimits,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine,
            access,
            security,
            limits,
            budget: Arc::new(Semaphore::new(limits.total_buffer_bytes)),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            upload_slots: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn limits(&self) -> &HlsIngestLimits {
        &self.limits
    }

    /// Resolve the stream key exactly as RTMP and SRT publish do.
    pub async fn authorize(
        &self,
        stream_key: &str,
        client_ip: &str,
    ) -> Result<AuthenticatedPipeline, AuthorizeError> {
        if self
            .security
            .is_ip_banned_for(RateLimitScope::HlsPublish, client_ip)
            .is_some()
        {
            return Err(AuthorizeError::Forbidden);
        }
        match self
            .access
            .authenticate(PipelineAccessMode::HlsPublish, stream_key, client_ip)
            .await
        {
            Ok(pipeline) => Ok(pipeline),
            Err(PipelineAccessError::InvalidStreamKey) => {
                warn!(stream_key = %redact_secret(stream_key), "hls ingest stream key rejected");
                Err(AuthorizeError::Forbidden)
            }
            Err(PipelineAccessError::LookupFailed(error)) => {
                error!(%error, "hls ingest stream key lookup failed");
                Err(AuthorizeError::LookupFailed(error))
            }
        }
    }

    /// One of the key's upload slots, held from body read to reply. `None`
    /// only if the slot semaphore was closed, which this module never does.
    pub async fn upload_slot(&self, input_id: &str) -> Option<OwnedSemaphorePermit> {
        let slots = {
            let mut slots = crate::sync::lock(&self.upload_slots);
            slots
                .entry(input_id.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(self.limits.uploads_per_key)))
                .clone()
        };
        slots.acquire_owned().await.ok()
    }

    /// An empty body for `kind`, sized from the declared length when known.
    pub fn object_body(
        &self,
        kind: ObjectKind,
        content_length: Option<u64>,
    ) -> Result<ObjectBody, BodyError> {
        let max_bytes = kind.max_bytes(&self.limits);
        let declared = content_length.map_or(Ok(0), |length| {
            usize::try_from(length)
                .ok()
                .filter(|length| *length <= max_bytes)
                .ok_or(BodyError::TooLarge)
        })?;
        Ok(ObjectBody {
            budget: self.budget.clone(),
            max_bytes,
            bytes: bytes::BytesMut::with_capacity(declared),
            permit: None,
        })
    }

    /// Hand one fully read object to the key's session, starting it if
    /// needed. A session that ended under the object gets one restart with
    /// the same object; a refused start (another publisher) is final.
    pub async fn dispatch(
        self: &Arc<Self>,
        pipeline: &AuthenticatedPipeline,
        stream_key: &str,
        client_addr: &str,
        kind: ObjectKind,
        path: String,
        body: ObjectBody,
    ) -> Result<ObjectOutcome, DispatchError> {
        let (reply, outcome) = oneshot::channel();
        let mut object = HlsObject {
            path,
            kind,
            payload: body.into_payload(),
            reply,
        };
        for _attempt in 0..2 {
            let (attempt_id, objects) = self.session_for(pipeline, stream_key, client_addr).await?;
            match objects.send(object).await {
                Ok(()) => return outcome.await.map_err(|_| DispatchError::Unavailable),
                Err(mpsc::error::SendError(returned)) => {
                    object = returned;
                    self.forget(&pipeline.input_id, attempt_id).await;
                }
            }
        }
        Err(DispatchError::Unavailable)
    }

    async fn session_for(
        self: &Arc<Self>,
        pipeline: &AuthenticatedPipeline,
        stream_key: &str,
        client_addr: &str,
    ) -> Result<(u64, mpsc::Sender<HlsObject>), DispatchError> {
        let mut sessions = self.sessions.lock().await;
        if let Some(handle) = sessions.get(&pipeline.input_id) {
            return Ok((handle.attempt_id, handle.objects.clone()));
        }
        let engine = &self.engine;
        let registration = engine
            .try_register_pipeline_input_attempt(
                &pipeline.id,
                &pipeline.input_id,
                stream_key,
                "hls",
                pipeline.selected,
            )
            .await
            .ok_or(DispatchError::PublisherActive)?;
        let ring = engine.get_or_create_pipeline(&pipeline.id).await;
        engine
            .update_ingest_session_meta(
                &pipeline.id,
                &registration,
                None,
                None,
                Some(client_addr.to_string()),
            )
            .await;
        let (objects, receiver) = mpsc::channel(OBJECT_QUEUE);
        let attempt_id = registration.attempt_id;
        sessions.insert(
            pipeline.input_id.clone(),
            SessionHandle {
                attempt_id,
                objects: objects.clone(),
            },
        );
        info!(pipeline = %pipeline.id, input = %pipeline.input_id, "[hls] Ingest registered");
        let context = SessionContext {
            engine: engine.clone(),
            pipeline_id: pipeline.id.clone(),
            registration,
            ring,
            limits: SessionLimits {
                hold_segments: self.limits.hold_segments,
                hold_bytes: self.limits.hold_bytes,
                idle_floor: self.limits.idle_floor,
            },
        };
        tokio::spawn(Arc::clone(self).supervise(context, receiver));
        Ok((attempt_id, objects))
    }

    async fn forget(&self, input_id: &str, attempt_id: u64) {
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(input_id)
            .is_some_and(|handle| handle.attempt_id == attempt_id)
        {
            sessions.remove(input_id);
        }
        drop(sessions);
        let mut slots = crate::sync::lock(&self.upload_slots);
        if slots
            .get(input_id)
            .is_some_and(|slot| Arc::strong_count(slot) == 1)
        {
            slots.remove(input_id);
        }
    }

    /// Runs the session on MEDIA and, whatever ends it (ENDLIST, idle,
    /// cancellation, a failure or a panic, which the executor contains),
    /// reports the disconnect and releases the input attempt.
    async fn supervise(
        self: Arc<Self>,
        context: SessionContext,
        objects: mpsc::Receiver<HlsObject>,
    ) {
        let pipeline_id = context.pipeline_id.clone();
        let registration: IngestRegistration = context.registration.clone();
        let service = MediaService {
            class: MediaServiceClass::HlsIngest,
            pipeline_id: pipeline_id.clone(),
            stage: format!("hls-ingest:{}", registration.input_id),
        };
        let result = crate::media::executor::run_with_class(
            service,
            registration.cancel_token.clone(),
            session::run(context, objects),
        )
        .await;
        let (phase, reason, had_error) = match &result {
            Ok((SessionEnd::Ended, _)) => ("eof", "HLS playlist ended".to_string(), false),
            Ok((SessionEnd::Idle, _)) => ("idle", "HLS publisher went idle".to_string(), false),
            Ok((SessionEnd::Cancelled, _)) => ("cancel", "HLS ingest cancelled".to_string(), false),
            Ok((SessionEnd::Failed(error), _)) => ("media-executor", error.clone(), true),
            Err(error) => ("media-executor", error.clone(), true),
        };
        if let Ok((_, counters)) = &result {
            info!(
                pipeline = %pipeline_id,
                phase,
                skipped = counters.skipped,
                duplicates = counters.duplicates,
                evicted = counters.evicted,
                epochs = counters.epochs,
                "[hls] Ingest session ended"
            );
        }
        self.engine
            .record_ingest_disconnect_if_current(
                &pipeline_id,
                &registration,
                Some(phase),
                Some(reason),
                had_error,
            )
            .await;
        self.engine
            .unregister_ingest_if_current(&pipeline_id, &registration)
            .await;
        self.forget(&registration.input_id, registration.attempt_id)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::hls_ingest::test_support::{
        INPUT_ID, PIPELINE_ID, STREAM_KEY, authenticated, test_ingest,
    };

    /// A session that ended between lookup and send gets exactly one
    /// restart, with the object already read.
    #[tokio::test]
    async fn an_object_for_a_session_that_just_ended_restarts_it_once() {
        let (engine, ingest) = test_ingest(HlsIngestLimits::default());
        let (stale, receiver) = mpsc::channel(OBJECT_QUEUE);
        drop(receiver);
        ingest.sessions.lock().await.insert(
            INPUT_ID.to_string(),
            SessionHandle {
                attempt_id: u64::MAX,
                objects: stale,
            },
        );
        let mut body = ingest.object_body(ObjectKind::Segment, None).unwrap();
        body.push(b"segment").unwrap();

        let outcome = ingest
            .dispatch(
                &authenticated(),
                STREAM_KEY,
                "127.0.0.1:9",
                ObjectKind::Segment,
                "index0.ts".to_string(),
                body,
            )
            .await;

        assert_eq!(outcome, Ok(ObjectOutcome::Held));
        let attempt = ingest.sessions.lock().await[INPUT_ID].attempt_id;
        assert_ne!(attempt, u64::MAX, "the stale handle was replaced");
        assert!(engine.input_has_session(PIPELINE_ID, INPUT_ID).await);
        engine.cancel_pipeline_input(INPUT_ID).await;
    }
}
