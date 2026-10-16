//! Stream-key authorization, per-key upload slots, the global buffer budget
//! and the session registry. Runs on Tokio CONTROL; per object it does one
//! lookup and one bounded channel send, never media work.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tracing::{error, info, warn};

use super::ordering::SessionLimits;
use super::session::{self, HlsObject, InitCache, SessionContext, SessionEnd};
use super::{
    BodyError, HlsIngestLimits, KeyCharge, ObjectBody, ObjectKind, ObjectOutcome, SegmentShare,
};
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
/// Initial body capacity; larger bodies grow with the bytes counted.
const INITIAL_BODY_CAPACITY: usize = 1024 * 1024;

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

/// Per stream key (input id): its upload slots and the lock that keeps two
/// first objects from both registering an attempt.
struct KeyState {
    uploads: Arc<Semaphore>,
    /// The key's share of the global budget, claimed by segments.
    share: Arc<Semaphore>,
    /// Serializes share claims; a chunked body holds it until complete.
    share_lock: Arc<tokio::sync::Mutex<()>>,
    /// Bytes the key has outstanding, for the fair-share log.
    outstanding: Arc<AtomicUsize>,
    /// The session's pacing lead in ms (target duration, or the idle floor
    /// before a playlist); sizes the whole-body deadline.
    lead_ms: Arc<AtomicU64>,
    start: tokio::sync::Mutex<()>,
}

pub struct HlsIngest {
    engine: Arc<MediaEngine>,
    access: Arc<dyn PipelineAccessAuthenticator>,
    security: Arc<IngestSecurityService>,
    limits: HlsIngestLimits,
    budget: Arc<Semaphore>,
    /// Keyed by input id; held for lookups only, never across an await.
    sessions: std::sync::Mutex<HashMap<String, SessionHandle>>,
    keys: std::sync::Mutex<HashMap<String, Arc<KeyState>>>,
    /// The init each stream key applied last: an encoder uploads its
    /// `EXT-X-MAP` init once per run, so it outlives the session.
    inits: Arc<std::sync::Mutex<InitCache>>,
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
            sessions: std::sync::Mutex::new(HashMap::new()),
            keys: std::sync::Mutex::new(HashMap::new()),
            inits: Arc::new(std::sync::Mutex::new(InitCache::new(
                limits.init_cache_bytes,
            ))),
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

    fn key_state(&self, input_id: &str) -> Arc<KeyState> {
        crate::sync::lock(&self.keys)
            .entry(input_id.to_string())
            .or_insert_with(|| {
                Arc::new(KeyState {
                    uploads: Arc::new(Semaphore::new(self.limits.uploads_per_key)),
                    share: Arc::new(Semaphore::new(self.limits.key_buffer_bytes)),
                    share_lock: Arc::new(tokio::sync::Mutex::new(())),
                    outstanding: Arc::new(AtomicUsize::new(0)),
                    lead_ms: Arc::new(AtomicU64::new(0)),
                    start: tokio::sync::Mutex::new(()),
                })
            })
            .clone()
    }

    /// One of the key's upload slots, held from body read to reply. `None`
    /// only if the slot semaphore was closed, which this module never does.
    pub async fn upload_slot(&self, input_id: &str) -> Option<OwnedSemaphorePermit> {
        let uploads = self.key_state(input_id).uploads.clone();
        uploads.acquire_owned().await.ok()
    }

    /// How long one request body may take in total: four target durations
    /// (four idle floors before the first playlist, and never less). A
    /// segment fits when the uplink carries a quarter of the stream bitrate,
    /// which a real-time encoder's uplink always does; a trickle does not.
    pub fn body_deadline(&self, input_id: &str) -> Duration {
        let lead = Duration::from_millis(self.key_state(input_id).lead_ms.load(Ordering::Relaxed));
        lead.max(self.limits.idle_floor).saturating_mul(4)
    }

    /// An empty body for `kind`, counted against `input_id`. A segment's
    /// declared length is claimed from the key's share and the global budget
    /// at once (waiting for the share); it preallocates at most 1 MiB, so
    /// memory tracks the bytes claimed.
    pub async fn object_body(
        &self,
        kind: ObjectKind,
        content_length: Option<u64>,
        input_id: &str,
    ) -> Result<ObjectBody, BodyError> {
        let max_bytes = kind.max_bytes(&self.limits);
        let declared = content_length.map_or(Ok(0), |length| {
            usize::try_from(length)
                .ok()
                .filter(|length| *length <= max_bytes)
                .ok_or(BodyError::TooLarge)
        })?;
        let key = self.key_state(input_id);
        let segment = (kind != ObjectKind::Playlist).then(|| SegmentShare {
            share: key.share.clone(),
            lock: key.share_lock.clone(),
            growing: None,
        });
        let mut body = ObjectBody {
            budget: self.budget.clone(),
            segment,
            max_bytes,
            bytes: bytes::BytesMut::with_capacity(declared.min(INITIAL_BODY_CAPACITY)),
            claimed: 0,
            permit: None,
            share_permit: None,
            input_id: input_id.to_string(),
            fair_share: self.limits.total_buffer_bytes / 4,
            charge: KeyCharge {
                outstanding: key.outstanding.clone(),
                bytes: 0,
            },
        };
        body.claim_declared(declared).await?;
        Ok(body)
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
                    self.forget(&pipeline.input_id, attempt_id);
                }
            }
        }
        Err(DispatchError::Unavailable)
    }

    fn current_session(&self, input_id: &str) -> Option<(u64, mpsc::Sender<HlsObject>)> {
        crate::sync::lock(&self.sessions)
            .get(input_id)
            .map(|handle| (handle.attempt_id, handle.objects.clone()))
    }

    async fn session_for(
        self: &Arc<Self>,
        pipeline: &AuthenticatedPipeline,
        stream_key: &str,
        client_addr: &str,
    ) -> Result<(u64, mpsc::Sender<HlsObject>), DispatchError> {
        if let Some(session) = self.current_session(&pipeline.input_id) {
            return Ok(session);
        }
        // Only objects of this key wait here; other keys start in parallel.
        let key = self.key_state(&pipeline.input_id);
        let _starting = key.start.lock().await;
        if let Some(session) = self.current_session(&pipeline.input_id) {
            return Ok(session);
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
        crate::sync::lock(&self.sessions).insert(
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
            lead_ms: key.lead_ms.clone(),
            limits: SessionLimits {
                hold_segments: self.limits.hold_segments,
                hold_bytes: self.limits.hold_bytes,
                idle_floor: self.limits.idle_floor,
            },
            max_init_bytes: self.limits.max_init_bytes,
            inits: self.inits.clone(),
        };
        tokio::spawn(Arc::clone(self).supervise(context, receiver));
        Ok((attempt_id, objects))
    }

    fn forget(&self, input_id: &str, attempt_id: u64) {
        let mut sessions = crate::sync::lock(&self.sessions);
        if sessions
            .get(input_id)
            .is_some_and(|handle| handle.attempt_id == attempt_id)
        {
            sessions.remove(input_id);
        }
        let idle = !sessions.contains_key(input_id);
        drop(sessions);
        let mut keys = crate::sync::lock(&self.keys);
        // Keep the key's state while anything still uses it: an upload slot
        // in flight, bytes counted against it, or a start in progress.
        if idle
            && keys.get(input_id).is_some_and(|key| {
                Arc::strong_count(key) == 1
                    && Arc::strong_count(&key.uploads) == 1
                    && Arc::strong_count(&key.share) == 1
                    && Arc::strong_count(&key.share_lock) == 1
                    && Arc::strong_count(&key.outstanding) == 1
                    && Arc::strong_count(&key.lead_ms) == 1
            })
        {
            keys.remove(input_id);
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
        self.forget(&registration.input_id, registration.attempt_id);
    }
}

/// Tests live in `registry_tests.rs` (this file is at the line cap).
#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
