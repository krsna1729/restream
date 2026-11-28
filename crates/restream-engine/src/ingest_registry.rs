//! Ingest state owned by `MediaEngine`: pipeline rings, input sessions and
//! their selection, the records stored in them, and the queue of dropped
//! ingest leases' attempts awaiting release. Session transactions are in
//! `lifecycle`, metadata, track and sequence-header operations are in
//! `metadata`, and ring creation and resizing are in `rings`.
//!
//! The registry access rules in the `engine_registries` module docs apply.
//! The few sections that must hold more than one ingest guard take them in
//! this order, and never the reverse:
//!
//! `selection` (one pipeline's) -> `cancel_tokens` -> `sessions` ->
//! `selected_inputs` -> `active`.
//!
//! The orders that span registries (a MoQ origin's per-pipeline attach lock
//! before that pipeline's `selection`, egress unregistration's `try_read` of
//! `active`, and `ingests.sessions` -> file-ingest `children` -> `active` ->
//! `positions`) are in the `engine_registries` module docs.
//!
//! `ingests.selection` is a [`KeyedLock`] keyed by pipeline id: it serializes
//! one pipeline's ingest lifecycle (registration replacement, selection,
//! track replacement with ring adaptation, origin resolution, disconnect
//! outcomes, release), so a holder stuck on one pipeline delays no other.
//! No section holds two pipelines' selections. The engine-wide ingest maps
//! under it are never held across a wait on media (a gate going idle), and
//! the maps keyed by input id (`cancel_tokens`, `sessions`) are checked and
//! changed under one write guard set, so two pipelines naming one input id
//! cannot interleave a check and its change.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use arc_swap::ArcSwapOption;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_sync::RingBuffer;
use restream_sync::keyed_lock::KeyedLock;
use tokio::sync::{Mutex as TokioMutex, Notify, RwLock as TokioRwLock};
use tokio_util::sync::CancellationToken;

use crate::events::{EventKind, EventLog};
use crate::input_gate::InputPacketGate;
use crate::snapshots::PublisherQuality;
use crate::stage_metrics::StageMetrics;

mod lifecycle;
mod metadata;
mod rings;

pub(crate) const INGEST_FLAP_WINDOW_MS: u64 = 30_000;

/// How long a source may publish media without reporting a probe before an
/// output stops waiting for one and starts video-only.
///
/// Ten seconds: longer than a long GOP, so an SRT publisher that reconnects
/// mid-GOP and completes its TS probe on the next SPS is still covered, and
/// longer than an HLS target duration. A source that never probes is the
/// exception this bound exists for; starting it a few seconds late costs far
/// less than a silent run without audio.
pub(crate) const PROBE_GRACE_MS: u64 = 10_000;

/// The failure phase recorded for an attempt whose lease was dropped without
/// `IngestLease::finish`.
pub const ABANDONED_PHASE: &str = "abandoned";
const ABANDONED_REASON: &str =
    "the input attempt ended without an orderly finish (its task panicked or was cancelled)";

pub struct IngestRegistry {
    pub pipelines: TokioRwLock<HashMap<String, Arc<RingBuffer>>>,
    pub(crate) cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
    pub(crate) next_attempt_id: AtomicU64,
    pub sessions: TokioRwLock<HashMap<String, Arc<ActiveIngest>>>,
    pub(crate) selected_inputs: TokioRwLock<HashMap<String, String>>,
    pub(crate) timelines: TokioRwLock<HashMap<String, Arc<AtomicI64>>>,
    preview_slots: TokioRwLock<HashMap<String, Arc<ArcSwapOption<RingBuffer>>>>,
    /// One lock per pipeline id over that pipeline's ingest lifecycle
    /// (module docs).
    pub(crate) selection: KeyedLock,
    pub preview_lock: TokioMutex<()>,
    pub active: TokioRwLock<HashMap<String, Arc<ActiveIngest>>>,
    pub(crate) recent: TokioRwLock<HashMap<String, RecentIngestOutcome>>,
    /// The `srt_call` inputs the SRT server dials.
    pub srt_calls: restream_srt::calls::SrtCallInputs,
    /// Dropped ingest leases' attempts awaiting release (`ingest_lease`),
    /// and the task releasing them.
    pub(crate) releases: Arc<ReleaseQueue>,
    release_drain: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The engine's event log (`RuntimeInfra::event_log`).
    events: Arc<EventLog>,
    /// A new source or preview ring's capacity, and the floor of a resize
    /// (`EngineConfig::ring_capacity`).
    ring_capacity: usize,
    /// The jitter a resized source ring holds, in seconds
    /// (`EngineConfig::ring_headroom_secs`).
    ring_headroom_secs: f64,
}

impl IngestRegistry {
    pub fn new(events: Arc<EventLog>, ring_capacity: usize, ring_headroom_secs: f64) -> Self {
        Self {
            pipelines: TokioRwLock::new(HashMap::new()),
            cancel_tokens: TokioRwLock::new(HashMap::new()),
            next_attempt_id: AtomicU64::new(1),
            sessions: TokioRwLock::new(HashMap::new()),
            selected_inputs: TokioRwLock::new(HashMap::new()),
            timelines: TokioRwLock::new(HashMap::new()),
            preview_slots: TokioRwLock::new(HashMap::new()),
            selection: KeyedLock::new(),
            preview_lock: TokioMutex::new(()),
            active: TokioRwLock::new(HashMap::new()),
            recent: TokioRwLock::new(HashMap::new()),
            srt_calls: Default::default(),
            releases: Arc::default(),
            release_drain: std::sync::Mutex::new(None),
            events,
            ring_capacity,
            ring_headroom_secs,
        }
    }

    pub async fn with_ingest_session<R>(
        &self,
        registration: &IngestRegistration,
        f: impl FnOnce(&ActiveIngest) -> R,
    ) -> Option<R> {
        self.current_ingest_session(registration)
            .await
            .map(|ingest| f(ingest.as_ref()))
    }

    pub(crate) async fn current_ingest_session(
        &self,
        registration: &IngestRegistration,
    ) -> Option<Arc<ActiveIngest>> {
        let sessions = self.sessions.read().await;
        let ingest = sessions.get(&registration.input_id)?;
        (ingest.attempt_id == registration.attempt_id).then(|| ingest.clone())
    }

    /// Inspect the session and update file-ingest state without letting
    /// registration replace the attempt between the check and the update.
    /// The operation may take file-ingest guards, but must not await ingest.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order `ingests.sessions` -> file-ingest children, active, positions (engine_registries module docs); the session is pinned through the update"
    )]
    pub(crate) async fn with_input_session_locked<R>(
        &self,
        input_id: &str,
        update_file_ingest: impl AsyncFnOnce(Option<&ActiveIngest>) -> R,
    ) -> R {
        let sessions = self.sessions.read().await;
        let result = update_file_ingest(sessions.get(input_id).map(Arc::as_ref)).await;
        drop(sessions);
        result
    }

    /// The live session registered for one input.
    pub(crate) async fn ingest_session(&self, input_id: &str) -> Option<Arc<ActiveIngest>> {
        self.sessions.read().await.get(input_id).cloned()
    }

    /// The selected active ingest for a pipeline.
    pub(crate) async fn active_ingest(&self, pipeline_id: &str) -> Option<Arc<ActiveIngest>> {
        self.active.read().await.get(pipeline_id).cloned()
    }

    /// Best-effort presence check for callers that hold an egress guard.
    pub(crate) fn try_has_active_ingest(&self, pipeline_id: &str) -> bool {
        self.active
            .try_read()
            .is_ok_and(|active| active.contains_key(pipeline_id))
    }

    pub async fn is_ingest_session_selected(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
    ) -> bool {
        self.active
            .read()
            .await
            .get(pipeline_id)
            .is_some_and(|ingest| ingest.attempt_id == registration.attempt_id)
    }

    pub async fn with_active_ingest<R>(
        &self,
        pipeline_id: &str,
        f: impl FnOnce(&ActiveIngest) -> R,
    ) -> Option<R> {
        let ingests = self.active.read().await;
        ingests.get(pipeline_id).map(|ingest| f(ingest.as_ref()))
    }

    pub async fn has_active_ingest(&self, pipeline_id: &str) -> bool {
        self.active.read().await.contains_key(pipeline_id)
    }

    /// Runs `f` over every selected ingest, keyed by pipeline id, under the
    /// `active` read guard; `f` must not wait.
    pub async fn with_active_ingests<R>(
        &self,
        f: impl FnOnce(&HashMap<String, Arc<ActiveIngest>>) -> R,
    ) -> R {
        f(&*self.active.read().await)
    }

    /// Runs `f` over every live input session, keyed by input id, under the
    /// `sessions` read guard; `f` must not wait.
    pub async fn with_ingest_sessions<R>(
        &self,
        f: impl FnOnce(&HashMap<String, Arc<ActiveIngest>>) -> R,
    ) -> R {
        f(&*self.sessions.read().await)
    }

    /// Runs `f` over every source ring, keyed by pipeline (or input preview
    /// resource) id, under the `pipelines` read guard; `f` must not wait.
    pub async fn with_source_rings<R>(
        &self,
        f: impl FnOnce(&HashMap<String, Arc<RingBuffer>>) -> R,
    ) -> R {
        f(&*self.pipelines.read().await)
    }

    /// [`Self::with_source_rings`] without waiting: `None` while the ring map
    /// is being written (a ring created, resized or removed).
    pub fn try_with_source_rings<R>(
        &self,
        f: impl FnOnce(&HashMap<String, Arc<RingBuffer>>) -> R,
    ) -> Option<R> {
        Some(f(&*self.pipelines.try_read().ok()?))
    }

    /// The pipeline's source ring, if one exists.
    pub async fn source_ring(&self, pipeline_id: &str) -> Option<Arc<RingBuffer>> {
        self.pipelines.read().await.get(pipeline_id).cloned()
    }

    /// Every pipeline's last ingest disconnect outcome, copied.
    pub async fn recent_ingest_outcomes(&self) -> HashMap<String, RecentIngestOutcome> {
        self.recent.read().await.clone()
    }

    /// Runs `f` on the pipeline's source ring with that pipeline's
    /// `selection` held, so what `f` reads of the ring and the selected
    /// ingest comes from one selected input. `None` when the pipeline has no
    /// ring. `f` must not take this pipeline's `selection`, directly or
    /// through an ingest operation (the lock is not reentrant), and must not
    /// hold an ingest map across its awaits.
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "this pipeline's `selection` keeps ring and track set from one selected input; under it each ingest map is taken one at a time (module docs)"
    )]
    pub async fn with_selected_source_ring<T>(
        &self,
        pipeline_id: &str,
        f: impl AsyncFnOnce(Arc<RingBuffer>) -> T,
    ) -> Option<T> {
        let _selection = self.selection.lock(pipeline_id).await;
        let ring = self.pipelines.read().await.get(pipeline_id).cloned()?;
        Some(f(ring).await)
    }

    /// The ingest a stage keyed by `stage_pipeline` reads: the pipeline's
    /// live ingest, or for an input preview's stage (`__input__:<input>`)
    /// that input's session.
    pub(crate) async fn stage_source_ingest(
        &self,
        stage_pipeline: &str,
    ) -> Option<Arc<ActiveIngest>> {
        match crate::preview_ids::input_id_from_hls_preview_resource_id(stage_pipeline) {
            Some(input_id) => self.sessions.read().await.get(input_id).cloned(),
            None => self.active.read().await.get(stage_pipeline).cloned(),
        }
    }

    /// Starts the task that ends dropped ingest leases' attempts on the
    /// current Tokio runtime, unless one runs; it first empties the backlog.
    /// Bootstrap starts it on the main runtime. A registration starts it when
    /// none runs (tests, or a task whose runtime stopped): an attempt queued
    /// meanwhile stays queued until then.
    pub fn start_release_drain(self: &Arc<Self>) {
        let mut drain = restream_sync::lock::lock(&self.release_drain);
        if drain.as_ref().is_some_and(|task| !task.is_finished()) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        *drain = Some(runtime.spawn(drain_releases(Arc::downgrade(self), self.releases.clone())));
    }

    /// Shutdown, once no session remains: stops the release task, then ends
    /// the backlog it left.
    pub async fn stop_release_drain(&self) {
        let task = restream_sync::lock::lock(&self.release_drain).take();
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        self.release_backlog(&self.releases).await;
    }

    /// Ends every queued attempt, oldest first. Each leaves the queue only
    /// once ended, so a pass cancelled midway leaves it for the next one, and
    /// two passes over one queue release every entry.
    pub(crate) async fn release_backlog(&self, queue: &ReleaseQueue) {
        while let Some(attempt) = queue.front() {
            self.release(&attempt).await;
            queue.complete(&attempt);
        }
    }

    /// Record (unless `finish` already did) and unregister one attempt;
    /// attempt-checked, so repeating it is harmless.
    async fn release(&self, attempt: &AbandonedAttempt) {
        if attempt.record {
            self.record_ingest_disconnect_if_current(
                &attempt.pipeline_id,
                &attempt.registration,
                Some(ABANDONED_PHASE),
                Some(ABANDONED_REASON.to_string()),
                true,
            )
            .await;
        }
        self.unregister_ingest_if_current(&attempt.pipeline_id, &attempt.registration)
            .await;
    }

    pub async fn record_ingest_disconnect(
        &self,
        pipeline_id: &str,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
    ) {
        let ingest = self.active.read().await.get(pipeline_id).cloned();
        let Some(ingest) = ingest else {
            return;
        };

        let previous = self.recent.read().await.get(pipeline_id).cloned();
        let metadata = ingest.metadata();
        let snapshot = Self::build_recent_ingest_outcome(
            previous.as_ref(),
            ingest.protocol.clone(),
            phase,
            reason,
            had_error,
            metadata.remote_addr.clone(),
            ingest.bytes_received.load(Ordering::Relaxed),
        );

        self.recent
            .write()
            .await
            .insert(pipeline_id.to_string(), snapshot);
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "this pipeline's `selection` keeps the current-attempt check and the outcome together; under it each ingest map is taken one at a time (module docs)"
    )]
    pub(crate) async fn record_ingest_disconnect_if_current(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
    ) -> bool {
        let _selection = self.selection.lock(pipeline_id).await;
        let ingest = self
            .sessions
            .read()
            .await
            .get(&registration.input_id)
            .cloned();
        let Some(ingest) = ingest else {
            return false;
        };
        if ingest.pipeline_id != pipeline_id || ingest.attempt_id != registration.attempt_id {
            return false;
        }

        let is_selected = self
            .active
            .read()
            .await
            .get(pipeline_id)
            .is_some_and(|active| active.attempt_id == registration.attempt_id);
        if !is_selected {
            return true;
        }

        let previous = self.recent.read().await.get(pipeline_id).cloned();
        let metadata = ingest.metadata();
        let snapshot = Self::build_recent_ingest_outcome(
            previous.as_ref(),
            ingest.protocol.clone(),
            phase,
            reason,
            had_error,
            metadata.remote_addr.clone(),
            ingest.bytes_received.load(Ordering::Relaxed),
        );

        self.recent
            .write()
            .await
            .insert(pipeline_id.to_string(), snapshot);
        true
    }

    /// Unregisters one attempt, if it is still its input's current attempt.
    /// Every write guard is taken first, in registration's lock order
    /// (`cancel_tokens` -> `sessions` -> `active`), and no await follows the
    /// first removal: a caller cancelled at an await leaves the attempt
    /// either whole (a retry ends it) or gone with its token cancelled, never
    /// a session removed with its token still live (the input then refused
    /// as a duplicate forever, the retry finding nothing to end).
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order `selection` (this pipeline's) -> `cancel_tokens` -> `sessions` -> `active` (module docs); every write guard is taken before the first removal (above)"
    )]
    pub(crate) async fn unregister_ingest_if_current(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
    ) -> bool {
        let _selection = self.selection.lock(pipeline_id).await;
        let mut tokens = self.cancel_tokens.write().await;
        let mut sessions = self.sessions.write().await;
        let mut active = self.active.write().await;
        let is_current = sessions.get(&registration.input_id).is_some_and(|ingest| {
            ingest.pipeline_id == pipeline_id && ingest.attempt_id == registration.attempt_id
        });
        if !is_current {
            return false;
        }
        sessions.remove(&registration.input_id);
        if let Some(token) = tokens.remove(&registration.input_id) {
            token.cancel();
        }
        let is_selected = active
            .get(pipeline_id)
            .is_some_and(|ingest| ingest.attempt_id == registration.attempt_id);
        let removed_selected = is_selected.then(|| active.remove(pipeline_id)).flatten();
        drop((tokens, sessions, active));
        let protocol = removed_selected
            .as_ref()
            .map(|ingest| ingest.protocol.clone())
            .unwrap_or_default();
        if let Some(ingest) = removed_selected {
            let remote_addr = restream_sync::lock::read(&ingest.metadata)
                .remote_addr
                .clone();
            let mut recent = self.recent.write().await;
            recent
                .entry(pipeline_id.to_string())
                .or_insert_with(|| RecentIngestOutcome {
                    protocol: ingest.protocol.clone(),
                    disconnected_at_ms: crate::timing::epoch_ms(),
                    first_disconnect_at_ms: crate::timing::epoch_ms(),
                    disconnect_count: 1,
                    reason: None,
                    failure_phase: None,
                    had_error: false,
                    remote_addr,
                    bytes_received: ingest.bytes_received.load(Ordering::Relaxed),
                });
        }

        if !protocol.is_empty() {
            self.events.emit(EventKind::IngestDisconnected {
                pipeline_id: pipeline_id.to_string(),
                protocol,
            });
        }
        true
    }

    pub(crate) fn build_recent_ingest_outcome(
        previous: Option<&RecentIngestOutcome>,
        protocol: String,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
        remote_addr: Option<String>,
        bytes_received: u64,
    ) -> RecentIngestOutcome {
        let disconnected_at_ms = crate::timing::epoch_ms();
        let (first_disconnect_at_ms, disconnect_count) = previous
            .filter(|previous| {
                disconnected_at_ms.saturating_sub(previous.disconnected_at_ms)
                    <= INGEST_FLAP_WINDOW_MS
            })
            .map(|previous| {
                (
                    previous.first_disconnect_at_ms,
                    previous.disconnect_count.saturating_add(1),
                )
            })
            .unwrap_or((disconnected_at_ms, 1));

        RecentIngestOutcome {
            protocol,
            disconnected_at_ms,
            first_disconnect_at_ms,
            disconnect_count,
            reason,
            failure_phase: phase.map(ToOwned::to_owned),
            had_error,
            remote_addr,
            bytes_received,
        }
    }
}

/// The registry's release task: on each wake, hands every unclaimed queued
/// attempt to a task of its own, until the registry is dropped.
async fn drain_releases(registry: Weak<IngestRegistry>, queue: Arc<ReleaseQueue>) {
    loop {
        let Some(strong) = registry.upgrade() else {
            return;
        };
        for (attempt, claim) in queue.claim_unclaimed() {
            let registry = strong.clone();
            let queue = queue.clone();
            tokio::spawn(async move {
                registry.release(&attempt).await;
                queue.complete(&attempt);
                drop(claim);
            });
        }
        drop(strong);
        queue.woken().await;
    }
}

#[derive(Clone, Default)]
pub struct IngestMetadata {
    pub remote_addr: Option<String>,
    pub video: Option<VideoMeta>,
    pub selected_video_track_index: Option<u32>,
    pub video_track_count: usize,
    pub audio: Option<AudioMeta>,
    pub quality: PublisherQuality,
    /// A container supplied a complete set, so missing tracks are absent,
    /// not merely unknown (as with an unprobed recirculation input).
    pub tracks_authoritative: bool,
    /// What an RTMP encoder declared in `onMetaData`, latest first seen.
    pub declared: Option<restream_core::metadata::DeclaredEncoder>,
}

pub struct ActiveIngest {
    pub attempt_id: u64,
    pub pipeline_id: String,
    pub input_id: String,
    pub stream_key: String,
    pub gate: Arc<InputPacketGate>,
    pub start_time: Instant,
    pub protocol: String,
    pub bytes_received: Arc<AtomicU64>,
    /// Epoch-ms of the session's first media byte, 0 until one arrives.
    /// The output start gate counts its probe grace from here, not from
    /// registration: a probe may legitimately complete long after the source
    /// starts sending (an SRT publisher reconnecting mid-GOP sends bytes at
    /// once but completes its TS probe only on the next SPS).
    pub first_media_ms: Arc<AtomicU64>,
    pub metrics: Arc<StageMetrics>,
    pub last_progress_ms: Arc<AtomicU64>,
    pub metadata: std::sync::RwLock<IngestMetadata>,
    pub audio_tracks: std::sync::Mutex<Arc<Vec<AudioMeta>>>,
    pub keyframe_times: Arc<std::sync::Mutex<Vec<i64>>>,
    /// Shared with the RTMP ingress owner, which updates and reads them
    /// synchronously while running the publisher's media.
    pub video_sequence_header: Arc<std::sync::Mutex<Option<bytes::Bytes>>>,
    pub audio_sequence_header: Arc<std::sync::Mutex<Option<bytes::Bytes>>>,
    pub prev_bytes_received: AtomicU64,
    pub prev_sample_time: std::sync::Mutex<Instant>,
    pub bitrate_kbps: std::sync::Mutex<Option<f64>>,
}

impl ActiveIngest {
    pub fn metadata(&self) -> IngestMetadata {
        restream_sync::lock::read(&self.metadata).clone()
    }
}

/// Count one media chunk's bytes and, on the session's first byte, the wall
/// clock the output start gate counts its probe grace from.
///
/// Called on the media path (per 64 KiB chunk for HLS push and file ingest,
/// per packet for RTMP and SRT). It reuses the bump that is already there
/// and the clock read the caller already makes for `last_progress_ms` (pass
/// that same `now_ms`): one comparison of the value `fetch_add` returns per
/// call, and one `compare_exchange` in the whole session.
pub(crate) fn note_media_bytes(
    bytes_received: &AtomicU64,
    first_media_ms: &AtomicU64,
    bytes: u64,
    now_ms: u64,
) {
    if bytes_received.fetch_add(bytes, Ordering::Relaxed) == 0 && bytes > 0 {
        let _ = first_media_ms.compare_exchange(0, now_ms, Ordering::Relaxed, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct IngestRegistration {
    pub cancel_token: CancellationToken,
    pub attempt_id: u64,
    pub input_id: String,
    pub gate: Arc<InputPacketGate>,
    pub last_forwarded_dts: Arc<AtomicI64>,
    pub preview_ring: Arc<ArcSwapOption<RingBuffer>>,
}

#[derive(Debug, Clone)]
pub struct RecentIngestOutcome {
    pub protocol: String,
    pub disconnected_at_ms: u64,
    pub first_disconnect_at_ms: u64,
    pub disconnect_count: u32,
    pub reason: Option<String>,
    pub failure_phase: Option<String>,
    pub had_error: bool,
    pub remote_addr: Option<String>,
    pub bytes_received: u64,
}

/// One dropped lease's attempt, for the release task.
#[derive(Clone)]
pub(crate) struct AbandonedAttempt {
    pub(crate) pipeline_id: String,
    pub(crate) registration: IngestRegistration,
    /// Whether to record the abandoned outcome: false once a cancelled
    /// `IngestLease::finish` already recorded its own.
    pub(crate) record: bool,
}

/// The registry's queue of dropped leases' attempts. Unbounded so a drop never
/// blocks or fails: it holds at most one entry per registration made, and
/// each entry leaves it only once its release completed.
#[derive(Default)]
pub(crate) struct ReleaseQueue {
    pending: std::sync::Mutex<Pending>,
    wake: Notify,
}

#[derive(Default)]
struct Pending {
    queue: VecDeque<AbandonedAttempt>,
    /// Attempts a release task is ending now, by attempt id.
    claimed: HashSet<u64>,
}

impl ReleaseQueue {
    pub(crate) fn push(&self, attempt: AbandonedAttempt) {
        restream_sync::lock::lock(&self.pending)
            .queue
            .push_back(attempt);
        // Stores a permit when no task waits, so a wake is never lost.
        self.wake.notify_one();
    }

    /// The oldest attempt not yet released; it stays queued until
    /// [`Self::complete`].
    pub(crate) fn front(&self) -> Option<AbandonedAttempt> {
        restream_sync::lock::lock(&self.pending)
            .queue
            .front()
            .cloned()
    }

    /// Every queued attempt no release task has, claimed now.
    pub(crate) fn claim_unclaimed(self: &Arc<Self>) -> Vec<(AbandonedAttempt, Claim)> {
        let mut pending = restream_sync::lock::lock(&self.pending);
        let Pending { queue, claimed } = &mut *pending;
        queue
            .iter()
            .filter(|attempt| claimed.insert(attempt.registration.attempt_id))
            .map(|attempt| {
                let claim = Claim {
                    queue: self.clone(),
                    attempt_id: attempt.registration.attempt_id,
                };
                (attempt.clone(), claim)
            })
            .collect()
    }

    /// Removes `attempt` by its id, wherever it is now: a second pass that
    /// released the same entry then removes nothing, never another entry.
    pub(crate) fn complete(&self, attempt: &AbandonedAttempt) {
        let mut pending = restream_sync::lock::lock(&self.pending);
        if let Some(index) = pending
            .queue
            .iter()
            .position(|queued| queued.registration.attempt_id == attempt.registration.attempt_id)
        {
            pending.queue.remove(index);
        }
    }

    pub(crate) async fn woken(&self) {
        self.wake.notified().await;
    }
}

/// One release task's claim on an attempt. Dropped, it frees the claim and
/// wakes the release task: an attempt still queued (its task cancelled) is
/// handed out again.
pub(crate) struct Claim {
    queue: Arc<ReleaseQueue>,
    attempt_id: u64,
}

impl Drop for Claim {
    fn drop(&mut self) {
        restream_sync::lock::lock(&self.queue.pending)
            .claimed
            .remove(&self.attempt_id);
        self.queue.wake.notify_one();
    }
}
