//! Stream-key authorization, per-key upload slots, the global buffer budget
//! and the session registry. Runs on Tokio CONTROL; per object it does one
//! lookup and one bounded channel send, never media work.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tracing::{error, info, warn};

use super::ordering::SessionLimits;
use super::session::{self, HlsObject, SessionContext, SessionEnd};
use super::{
    BodyError, HlsIngestLimits, KeyCharge, ObjectBody, ObjectKind, ObjectOutcome, SegmentShare,
};
use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::executor::{MediaService, MediaServiceClass};
use crate::media::hls_pull::{HlsPullTask, PullEnd, PullIdentity, PullSource};
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

/// Where an object came from. It decides the protocol recorded on the ingest
/// session: an upload is `hls`, and an object the engine fetched itself is
/// `hls-pull`. The two share one session, one ordering and one publisher, so
/// a pipeline reads the same whichever way its segments arrive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOrigin {
    Push,
    Pull,
}

impl DispatchOrigin {
    /// The protocol label recorded on the ingest session.
    pub const fn protocol(self) -> &'static str {
        match self {
            Self::Push => "hls",
            Self::Pull => "hls-pull",
        }
    }
}

struct SessionHandle {
    attempt_id: u64,
    objects: mpsc::Sender<HlsObject>,
}

/// The last init a stream key applied, and the `EXT-X-MAP` it belongs to.
/// FFmpeg uploads its `EXT-X-MAP` init once per run, so the key keeps it:
/// a session that starts after an idle timeout, a retry or a failure
/// applies the same init again. It lives here, with the rest of the key's
/// state, so it can never be evicted while the key exists, and there is at
/// most one per input that has pushed fMP4 HLS. Deleting an input does not
/// reach this registry, so a deleted input's init (at most
/// `max_init_bytes`) stays until the process restarts: no upload can name
/// that key again.
#[derive(Clone)]
pub(super) struct AppliedInit {
    pub(super) uri: String,
    pub(super) bytes: bytes::Bytes,
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
    /// The init the key applied last, across sessions.
    init: Arc<std::sync::Mutex<Option<AppliedInit>>>,
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
    /// One pull task per `hls_pull` input, with the source it was started
    /// for so a changed row restarts it. Held for lookups only, never across
    /// an await.
    pulls: std::sync::Mutex<HashMap<String, RunningPull>>,
}

/// A running pull and the source it was started from. A row whose URL,
/// rendition or credential differs is a different origin, so the task is
/// replaced rather than left pulling what the row no longer says.
struct RunningPull {
    source: PullSource,
    task: HlsPullTask,
    /// Set when the task ended fatally: the earliest instant a pass may start
    /// it again. `None` while it runs and after a restart.
    retry_at: Option<std::time::Instant>,
}

/// How long a pass waits before restarting a pull that ended fatally. The
/// puller already retries inside itself; this is the outer bound for a source
/// that gave up entirely (a wrong URL, refused credentials), so a broken row
/// costs one attempt per interval rather than a spin.
const PULL_RESTART_BACKOFF: Duration = Duration::from_secs(30);

/// What a pass should do with one input's pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PullAction {
    Leave,
    /// Start a task; `replaced` says whether one was running and must be
    /// cancelled, which also ends the session it opened.
    Start {
        replaced: bool,
    },
    /// The task ended fatally and its backoff has not elapsed.
    WaitToRetry,
}

/// Decide one input's pull from what the registry knows about it.
///
/// Pure so every branch is testable without a clock or an origin: the caller
/// supplies the current instant and records the retry deadline when the
/// answer is [`PullAction::WaitToRetry`].
fn pull_action(
    observed: Option<&PullObservation>,
    source: &PullSource,
    now: std::time::Instant,
) -> PullAction {
    let Some(observed) = observed else {
        return PullAction::Start { replaced: false };
    };
    if observed.source != *source {
        // The row names a different origin, so whatever the old task is
        // doing, it is not what the row says.
        return PullAction::Start { replaced: true };
    }
    if !observed.finished {
        return PullAction::Leave;
    }
    match observed.end {
        // A finished playlist is terminal for this source: restarting would
        // replay it.
        Some(PullEnd::Ended) => PullAction::Leave,
        // A source that gave up may recover, so wait out the backoff. So may
        // a task that panicked: the task records its end as its last act, and
        // a cancelled task is removed from the map before it is cancelled, so
        // a finished task with no recorded end is one that panicked. Leaving
        // it would keep the row's pull dead for as long as the row stands.
        Some(PullEnd::Fatal) | None => match observed.retry_at {
            Some(retry_at) if now >= retry_at => PullAction::Start { replaced: false },
            Some(_) | None => PullAction::WaitToRetry,
        },
    }
}

/// What the registry knows about an input's pull, apart from the task itself,
/// so [`pull_action`] can be read and tested on its own.
struct PullObservation {
    source: PullSource,
    finished: bool,
    end: Option<PullEnd>,
    retry_at: Option<std::time::Instant>,
}

impl RunningPull {
    fn observe(&self) -> PullObservation {
        PullObservation {
            source: self.source.clone(),
            finished: self.task.is_finished(),
            end: self.task.end(),
            retry_at: self.retry_at,
        }
    }
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
            pulls: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn limits(&self) -> &HlsIngestLimits {
        &self.limits
    }

    /// Ensure the input is pulling `source`, starting a task if none runs or
    /// if the running one was started for a different source.
    ///
    /// Returns whether a task was started, so the caller can log a change and
    /// a repeated reconcile pass costs one comparison.
    ///
    /// A replaced task takes its ingest session with it. The session holds
    /// the ordering state — the media sequence it has already seen, the
    /// timeline it is publishing against — and a new origin numbers its
    /// segments independently, so inheriting the old state would classify its
    /// first segments as duplicates or rewind the timeline. The next object
    /// the new task dispatches opens a fresh session.
    ///
    /// A task that ended fatally is reaped and started again behind
    /// [`PULL_RESTART_BACKOFF`]; one that ended at `EXT-X-ENDLIST` is left
    /// alone, because its playlist is complete and restarting would replay it.
    pub async fn ensure_pull(self: &Arc<Self>, identity: PullIdentity, source: PullSource) -> bool {
        let now = std::time::Instant::now();
        let action = {
            let pulls = crate::sync::lock(&self.pulls);
            let observed = pulls.get(&identity.input_id).map(RunningPull::observe);
            pull_action(observed.as_ref(), &source, now)
        };
        let replaced = match action {
            PullAction::Leave => return false,
            PullAction::WaitToRetry => {
                // Record the deadline once: the next pass that finds it
                // elapsed starts the task.
                let mut pulls = crate::sync::lock(&self.pulls);
                if let Some(running) = pulls.get_mut(&identity.input_id)
                    && running.retry_at.is_none()
                {
                    running.retry_at = Some(now + PULL_RESTART_BACKOFF);
                }
                return false;
            }
            PullAction::Start { replaced } => replaced,
        };
        // Take the old task out, whatever state it is in, and wait for it
        // before touching its session. A cancelled task must observe the
        // cancel before it can dispatch another object, or it would open the
        // very session the new source is about to replace — and, when the row
        // is gone, recreate the ring of a pipeline that is going away. The
        // join costs one poll, because the task's cancel branch is first in
        // its `select!`.
        //
        // The removal is its own statement so the map's guard is released
        // before the await, never held across it.
        let previous = crate::sync::lock(&self.pulls).remove(&identity.input_id);
        if let Some(previous) = previous {
            previous.task.cancel();
            let end = previous.task.end_state();
            previous.task.join().await;
            if crate::sync::lock(&end).is_none() {
                warn!(
                    input = %identity.input_id,
                    "HLS pull task stopped without recording why; restarting it"
                );
            }
            // A replaced task takes its session with it (see above). A task
            // restarted for the same source keeps it, so its ordering state
            // still dedupes whatever the retry replays.
            if replaced {
                self.engine.cancel_pipeline_input(&identity.input_id).await;
            }
        }
        let input_id = identity.input_id.clone();
        let task = crate::media::hls_pull::spawn(
            self.engine.clone(),
            self.clone(),
            identity,
            source.clone(),
            self.limits,
        );
        crate::sync::lock(&self.pulls).insert(
            input_id,
            RunningPull {
                source,
                task,
                retry_at: None,
            },
        );
        true
    }

    /// Stop the input's pull task, if one runs, and end the session it
    /// opened. The task is joined before its session goes, so it cannot
    /// dispatch another object after the cancel and reopen that session; its
    /// cancel branch is first in its `select!`, so the join costs one poll
    /// rather than an origin's request timeout.
    pub async fn cancel_pull(&self, input_id: &str) -> bool {
        // The removal is its own statement so the map's guard is released
        // before the join, never held across it.
        let removed = crate::sync::lock(&self.pulls).remove(input_id);
        match removed {
            Some(running) => {
                running.task.cancel();
                running.task.join().await;
                self.engine.cancel_pipeline_input(input_id).await;
                true
            }
            None => false,
        }
    }

    /// The inputs currently pulling.
    pub fn pull_input_ids(&self) -> HashSet<String> {
        crate::sync::lock(&self.pulls).keys().cloned().collect()
    }

    /// Stop every pull task. Called once, on shutdown.
    pub fn cancel_all_pulls(&self) {
        let running: Vec<_> = crate::sync::lock(&self.pulls).drain().collect();
        for (_, running) in running {
            running.task.cancel();
        }
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
                    init: Arc::new(std::sync::Mutex::new(None)),
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
    // Every argument is a distinct fact about the object or its session
    // (which pipeline, which key, from where, pushed or pulled, what kind,
    // which path, and the bytes); bundling them would only move the list.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch(
        self: &Arc<Self>,
        pipeline: &AuthenticatedPipeline,
        stream_key: &str,
        client_addr: &str,
        origin: DispatchOrigin,
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
            let (attempt_id, objects) = self
                .session_for(pipeline, stream_key, client_addr, origin)
                .await?;
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
        origin: DispatchOrigin,
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
                origin.protocol(),
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
            init: key.init.clone(),
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
        // in flight, bytes counted against it, a start in progress, or the
        // init the key applied last — FFmpeg uploads its `EXT-X-MAP` init
        // once per run, so an encoder that resumes after an idle timeout
        // needs it. The key state is that init's home, so it lives as long
        // as the init does: one per input that has pushed fMP4 (see
        // `AppliedInit` for a deleted input's).
        if idle
            && keys.get(input_id).is_some_and(|key| {
                crate::sync::lock(&key.init).is_none()
                    && Arc::strong_count(key) == 1
                    && Arc::strong_count(&key.uploads) == 1
                    && Arc::strong_count(&key.share) == 1
                    && Arc::strong_count(&key.share_lock) == 1
                    && Arc::strong_count(&key.outstanding) == 1
                    && Arc::strong_count(&key.lead_ms) == 1
                    && Arc::strong_count(&key.init) == 1
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

/// Tests live in `registry_tests.rs` (this file is at the line cap); the
/// pull-registry cases live in `pull_registry_tests.rs`, which keeps that
/// file's growth bounded too.
#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pull_registry_tests.rs"]
mod pull_tests;
