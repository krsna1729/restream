//! Ingest state owned by `MediaEngine`: pipeline rings, input sessions and
//! their selection, the records stored in them, and the queue of dropped
//! ingest leases' attempts awaiting release.
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
//! `active`, and file ingest's `sessions` -> `children` -> `active` ->
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
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Instant;

use arc_swap::ArcSwapOption;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_sync::RingBuffer;
use restream_sync::keyed_lock::KeyedLock;
use tokio::sync::{Mutex as TokioMutex, Notify, RwLock as TokioRwLock};
use tokio_util::sync::CancellationToken;

use crate::input_gate::InputPacketGate;
use crate::snapshots::PublisherQuality;
use crate::stage_metrics::StageMetrics;

pub struct IngestRegistry {
    pub pipelines: TokioRwLock<HashMap<String, Arc<RingBuffer>>>,
    pub cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
    pub next_attempt_id: AtomicU64,
    pub sessions: TokioRwLock<HashMap<String, Arc<ActiveIngest>>>,
    pub selected_inputs: TokioRwLock<HashMap<String, String>>,
    pub timelines: TokioRwLock<HashMap<String, Arc<AtomicI64>>>,
    pub preview_slots: TokioRwLock<HashMap<String, Arc<ArcSwapOption<RingBuffer>>>>,
    /// One lock per pipeline id over that pipeline's ingest lifecycle
    /// (module docs).
    pub selection: KeyedLock,
    pub preview_lock: TokioMutex<()>,
    pub active: TokioRwLock<HashMap<String, Arc<ActiveIngest>>>,
    pub recent: TokioRwLock<HashMap<String, RecentIngestOutcome>>,
    /// The `srt_call` inputs the SRT server dials.
    pub srt_calls: restream_srt::calls::SrtCallInputs,
    /// Dropped ingest leases' attempts awaiting release (`ingest_lease`),
    /// and the task releasing them.
    pub(crate) releases: Arc<ReleaseQueue>,
    pub(crate) release_drain: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Default for IngestRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl IngestRegistry {
    pub fn new() -> Self {
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
        }
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

/// One dropped lease's attempt, for the engine's release task.
#[derive(Clone)]
pub(crate) struct AbandonedAttempt {
    pub(crate) pipeline_id: String,
    pub(crate) registration: IngestRegistration,
    /// Whether to record the abandoned outcome: false once a cancelled
    /// `IngestLease::finish` already recorded its own.
    pub(crate) record: bool,
}

/// The engine's queue of dropped leases' attempts. Unbounded so a drop never
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
