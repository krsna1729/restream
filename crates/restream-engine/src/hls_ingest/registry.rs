//! Stream-key authorization, per-key upload slots, the global buffer budget
//! and the session registry. Runs on Tokio CONTROL; per object it does one
//! lookup and one bounded channel send, never media work.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tracing::{error, info, warn};

use super::ordering::{Counters, MAX_RENDITIONS, SessionLimits};
use super::session::{self, HlsObject, SessionContext, SessionEnd};
use super::{
    BodyError, HlsIngestLimits, KeyCharge, ObjectBody, ObjectKind, ObjectOutcome, SegmentShare,
};
use crate::engine::{IngestRegistration, MediaEngine};
use crate::executor::{MediaService, MediaServiceClass};
use crate::hls_pull::{HlsPullTask, PullEnd, PullIdentity, PullSource};
use crate::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessMode,
};
use crate::secret_display::redact_secret;
use crate::security::{IngestSecurityService, RateLimitScope};

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
    /// The stream key no longer authorizes this input: its row changed between
    /// the upload's authorization and the session it tried to open — disabled,
    /// deleted, or remapped to another input.
    AuthorizationChanged,
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
/// most one per followed rendition of an input that has pushed fMP4 HLS
/// (see `AppliedInits`). The reconciler releases it when the input's row is
/// gone (`HlsIngest::forget_input`), so a deleted input's init (at most
/// `max_init_bytes`) is not held until the process restarts.
#[derive(Clone)]
pub(super) struct AppliedInit {
    pub(super) uri: String,
    pub(super) bytes: bytes::Bytes,
}

/// The init each followed rendition applied last: the variant's and its
/// separate audio rendition's demuxers each hold their own.
#[derive(Clone, Default)]
pub(super) struct AppliedInits([Option<AppliedInit>; MAX_RENDITIONS]);

impl AppliedInits {
    pub(super) fn is_empty(&self) -> bool {
        self.0.iter().all(Option::is_none)
    }

    pub(super) fn set(&mut self, rendition: usize, init: AppliedInit) {
        if let Some(slot) = self.0.get_mut(rendition) {
            *slot = Some(init);
        }
    }

    /// Forget the init applied under `uri` (a refused re-upload of it).
    pub(super) fn forget_uri(&mut self, uri: &str) {
        for slot in &mut self.0 {
            if slot.as_ref().is_some_and(|init| init.uri == uri) {
                *slot = None;
            }
        }
    }

    /// Each rendition's init, by rendition.
    pub(super) fn iter(&self) -> impl Iterator<Item = (usize, &AppliedInit)> {
        self.0
            .iter()
            .enumerate()
            .filter_map(|(rendition, init)| init.as_ref().map(|init| (rendition, init)))
    }
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
    /// The inits the key applied last, across sessions.
    init: Arc<std::sync::Mutex<AppliedInits>>,
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

/// Releases an ingest attempt whose `session_for` future is dropped before a
/// session owner takes it over.
///
/// `session_for` registers the attempt and then awaits the pipeline and its
/// metadata before spawning the owner. A cancellation that lands in that window
/// drops the future: without this guard the engine keeps an attempt nothing
/// will end (a disabled input still shown as connected), and a session opened
/// for an input another source is about to own. The guard is disarmed once the
/// owner is spawned; the owner then releases the attempt itself.
struct RegistrationGuard {
    engine: Arc<MediaEngine>,
    pipeline_id: String,
    registration: IngestRegistration,
    armed: bool,
}

impl RegistrationGuard {
    fn new(
        engine: &Arc<MediaEngine>,
        pipeline_id: &str,
        registration: &IngestRegistration,
    ) -> Self {
        Self {
            engine: Arc::clone(engine),
            pipeline_id: pipeline_id.to_string(),
            registration: registration.clone(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RegistrationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (engine, pipeline_id, registration) = (
            Arc::clone(&self.engine),
            self.pipeline_id.clone(),
            self.registration.clone(),
        );
        // Drop cannot await. The attempt-scoped release runs on its own task,
        // so it cannot end a registration that replaced this one; outside a
        // runtime there is nothing to release.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                engine
                    .unregister_ingest_if_current(&pipeline_id, &registration)
                    .await;
            });
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
            let pulls = restream_sync::lock::lock(&self.pulls);
            let observed = pulls.get(&identity.input_id).map(RunningPull::observe);
            pull_action(observed.as_ref(), &source, now)
        };
        let replaced = match action {
            PullAction::Leave => return false,
            PullAction::WaitToRetry => {
                // Record the deadline once: the next pass that finds it
                // elapsed starts the task.
                let mut pulls = restream_sync::lock::lock(&self.pulls);
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
        let previous = restream_sync::lock::lock(&self.pulls).remove(&identity.input_id);
        if let Some(previous) = previous {
            previous.task.cancel();
            let end = previous.task.end_state();
            previous.task.join().await;
            if restream_sync::lock::lock(&end).is_none() {
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
        let task = crate::hls_pull::spawn(
            self.engine.clone(),
            self.clone(),
            identity,
            source.clone(),
            self.limits,
        );
        restream_sync::lock::lock(&self.pulls).insert(
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
    #[cfg(test)]
    pub async fn cancel_pull(&self, input_id: &str) -> bool {
        !self
            .cancel_pulls(std::slice::from_ref(&input_id.to_string()))
            .await
            .is_empty()
    }

    /// Stop several pulls, cancelling every task before awaiting any of them:
    /// a reconcile pass stops the pulls whose rows went away, and one stalled
    /// origin must not hold the pass (and with it every other pipeline's
    /// topology convergence) behind each join in turn.
    ///
    /// Every requested input is cleared afterwards, not only the ones whose
    /// pull was still running: a session that ended earlier leaves the init it
    /// applied behind. Returns the inputs that had a pull to stop.
    pub async fn cancel_pulls(&self, input_ids: &[String]) -> Vec<String> {
        let mut stopping = Vec::new();
        for input_id in input_ids {
            if let Some(running) = restream_sync::lock::lock(&self.pulls).remove(input_id) {
                running.task.cancel();
                stopping.push((input_id.clone(), running));
            }
        }
        let stopped: Vec<String> = stopping
            .iter()
            .map(|(input_id, _)| input_id.clone())
            .collect();
        futures_util::future::join_all(
            stopping.into_iter().map(|(_, running)| running.task.join()),
        )
        .await;
        // Only an input whose pull was running has a session to end here; the
        // init is cleared for every requested input, because a session that
        // ended earlier leaves the init it applied behind.
        for input_id in &stopped {
            self.engine.cancel_pipeline_input(input_id).await;
        }
        for input_id in input_ids {
            self.clear_applied_init(input_id);
        }
        stopped
    }

    fn clear_applied_init(&self, input_id: &str) {
        let keys = restream_sync::lock::lock(&self.keys);
        if let Some(key) = keys.get(input_id) {
            *restream_sync::lock::lock(&key.init) = AppliedInits::default();
        }
    }

    /// The inputs currently pulling.
    pub fn pull_input_ids(&self) -> HashSet<String> {
        restream_sync::lock::lock(&self.pulls)
            .keys()
            .cloned()
            .collect()
    }

    /// The inputs whose key state this registry holds.
    pub fn keyed_input_ids(&self) -> HashSet<String> {
        restream_sync::lock::lock(&self.keys)
            .keys()
            .cloned()
            .collect()
    }

    /// Release one input's key state, for an input whose row is gone.
    ///
    /// The applied init exists so an encoder that resumes after an idle
    /// timeout can publish without re-uploading it, and a deleted input has no
    /// encoder to resume: holding it would keep up to `max_init_bytes` per
    /// deleted fMP4 input until the process restarts.
    pub fn forget_input(&self, input_id: &str) {
        self.clear_applied_init(input_id);
        self.evict_key_if_idle(input_id);
    }

    /// Stop every pull task. Called once, on shutdown.
    pub fn cancel_all_pulls(&self) {
        let running: Vec<_> = restream_sync::lock::lock(&self.pulls).drain().collect();
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
        restream_sync::lock::lock(&self.keys)
            .entry(input_id.to_string())
            .or_insert_with(|| {
                Arc::new(KeyState {
                    uploads: Arc::new(Semaphore::new(self.limits.uploads_per_key)),
                    share: Arc::new(Semaphore::new(self.limits.key_buffer_bytes)),
                    share_lock: Arc::new(tokio::sync::Mutex::new(())),
                    outstanding: Arc::new(AtomicUsize::new(0)),
                    lead_ms: Arc::new(AtomicU64::new(0)),
                    init: Arc::new(std::sync::Mutex::new(AppliedInits::default())),
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
        let payload = body.into_payload();
        // A playlist is parsed here, on the blocking pool, so the session turn
        // that applies it on the shared MEDIA worker never parses: parsing is
        // linear in the playlist, about 29 ms for a 1 MiB one.
        let content = match kind {
            ObjectKind::Playlist => {
                let bytes = payload.bytes.clone();
                let playlist_path = path.clone();
                let parsed = tokio::task::spawn_blocking(move || {
                    super::playlist::parse(&bytes, &playlist_path)
                })
                .await
                .map_err(|_| DispatchError::Unavailable)?;
                session::ObjectContent::Playlist(parsed)
            }
            ObjectKind::TsSegment => session::ObjectContent::TsSegment,
            ObjectKind::Fmp4 => session::ObjectContent::Fmp4,
        };
        let mut object = HlsObject {
            path,
            content,
            payload,
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
        restream_sync::lock::lock(&self.sessions)
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
        // An upload was authorized before its body was read, and a body may
        // arrive up to four target durations later. Re-resolve the key here,
        // under the key's start lock, so an input disabled, deleted or changed
        // to a pull input in that window cannot open a session — or recreate
        // the ring of a pipeline that is going away — afterwards. A pull is
        // authorized by its own row on every pass, and its objects never carry
        // a stream key a peer supplied.
        //
        // This is a second authorization of the same attempt, so it also runs
        // the security accounting a second time: a success resets that IP's
        // failure count again (idempotent), and only the rare disable-or-remap
        // race adds a failure. Re-checking the input row directly instead would
        // put a second copy of the authorization rule in this module.
        // The row the key resolves to now is the one registered: the pipeline
        // captured before the body was read may have been renamed or deselected
        // since.
        let refreshed: AuthenticatedPipeline;
        let mut pipeline = pipeline;
        if origin == DispatchOrigin::Push {
            // The security layer keys bans and rate limits by bare IP, and the
            // ingest protocols pass it that way; `client_addr` is the peer
            // address the session meta records, so derive the IP from it.
            let client_ip = client_addr
                .parse::<SocketAddr>()
                .map_or_else(|_| client_addr.to_string(), |addr| addr.ip().to_string());
            match self.authorize(stream_key, &client_ip).await {
                Ok(current) if current.input_id == pipeline.input_id => {
                    refreshed = current;
                    pipeline = &refreshed;
                }
                Ok(_) => {
                    warn!(
                        input = %pipeline.input_id,
                        "hls ingest upload authorized for another input; refused"
                    );
                    return Err(DispatchError::AuthorizationChanged);
                }
                Err(AuthorizeError::Forbidden) => {
                    return Err(DispatchError::AuthorizationChanged);
                }
                Err(AuthorizeError::LookupFailed(error)) => {
                    error!(%error, "hls ingest stream key lookup failed at session start");
                    return Err(DispatchError::Unavailable);
                }
            }
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
        // From here until the owner is spawned below, the attempt has no owner:
        // a caller that drops this future in that window (a pull's cancel lands
        // in it) must release the registration, or the engine keeps an attempt
        // nothing will end — a disabled input still shown as connected — and the
        // ring is recreated under it.
        let mut guard = RegistrationGuard::new(engine, &pipeline.id, &registration);
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
        restream_sync::lock::lock(&self.sessions).insert(
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
        guard.disarm();
        Ok((attempt_id, objects))
    }

    fn forget(&self, input_id: &str, attempt_id: u64) {
        let mut sessions = restream_sync::lock::lock(&self.sessions);
        if sessions
            .get(input_id)
            .is_some_and(|handle| handle.attempt_id == attempt_id)
        {
            sessions.remove(input_id);
        }
        drop(sessions);
        self.evict_key_if_idle(input_id);
    }

    /// Evict a key's state once nothing uses it: no session for the input, no
    /// upload slot in flight, no bytes counted against it, no start in
    /// progress, and no init applied — FFmpeg uploads its `EXT-X-MAP` init once
    /// per run, so an encoder that resumes after an idle timeout needs it, and
    /// the key state is that init's home. It lives as long as the init does:
    /// one per input that has pushed fMP4 (see `AppliedInit`).
    fn evict_key_if_idle(&self, input_id: &str) {
        let idle = !restream_sync::lock::lock(&self.sessions).contains_key(input_id);
        let mut keys = restream_sync::lock::lock(&self.keys);
        if idle
            && keys.get(input_id).is_some_and(|key| {
                restream_sync::lock::lock(&key.init).is_empty()
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

    /// Seed an applied init for a test, as a session that applied one leaves
    /// it. A caller that needs to hold the key state takes it with `key_state`
    /// first; this seeds through the same entry.
    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_applied_init_for_tests(&self, input_id: &str) {
        let key = self.key_state(input_id);
        restream_sync::lock::lock(&key.init).set(
            0,
            AppliedInit {
                uri: "init.mp4".to_string(),
                bytes: bytes::Bytes::from_static(b"init"),
            },
        );
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
        self.supervise_session(pipeline_id, registration, session::run(context, objects))
            .await;
    }

    /// The supervision half of `supervise`, for any session future: run it as
    /// a contained MEDIA service, then report and release whatever ended it.
    async fn supervise_session(
        self: Arc<Self>,
        pipeline_id: String,
        registration: IngestRegistration,
        session: impl Future<Output = (SessionEnd, Counters)> + Send + 'static,
    ) {
        let service = MediaService {
            class: MediaServiceClass::HlsIngest,
            pipeline_id: pipeline_id.clone(),
            stage: format!("hls-ingest:{}", registration.input_id),
        };
        let result =
            crate::executor::run_with_class(service, registration.cancel_token.clone(), session)
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
/// pull-registry cases live in `pull_registry_tests.rs`, and the session-start
/// window in `session_start_tests.rs`, which keeps that file's growth bounded
/// too.
#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "key_state_tests.rs"]
mod key_state_tests;

#[cfg(test)]
#[path = "pull_registry_tests.rs"]
mod pull_tests;

#[cfg(test)]
#[path = "session_start_tests.rs"]
mod session_start_tests;

#[cfg(test)]
#[path = "session_end_tests.rs"]
mod session_end_tests;
