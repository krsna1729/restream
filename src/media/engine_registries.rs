//! Registry structs for `MediaEngine` state ownership, grouping the
//! synchronized maps and sets that back ingest, egress, HLS, and stage lifecycles.
//!
//! Registry access rule: do not hold registry guards across awaits that acquire
//! another registry. Snapshot paths should copy or render the fields they need
//! inside a bounded scope, drop all guards, then perform async enrichment such
//! as stage or HLS lookups. Lifecycle paths that must touch more than one
//! registry keep the critical section short and avoid calling out while guards
//! are live.

use arc_swap::ArcSwapOption;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as TokioMutex, RwLock as TokioRwLock};
use tokio_util::sync::CancellationToken;

use crate::domain::stage::StageKey;
use crate::events::EventLog;
use crate::media::avio::MemoryQueue;
use crate::media::engine::{
    ActiveEgress, ActiveIngest, EgressRetryState, HlsConsumers, ListenerSocketStats,
    RecentEgressOutcome, RecentIngestOutcome, RtmpListenerStats,
};
use crate::media::hls::HlsStore;
use crate::media::hls_fmp4::Fmp4HlsStore;
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_lifecycle::StageLifecycle;
use crate::media::stage_metrics::StageMetrics;
use crate::media::ts_chunk_ring::TsChunkRing;

pub struct IngestRegistry {
    pub pipelines: TokioRwLock<HashMap<String, Arc<RingBuffer>>>,
    pub cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
    pub next_attempt_id: AtomicU64,
    pub sessions: TokioRwLock<HashMap<String, Arc<ActiveIngest>>>,
    pub selected_inputs: TokioRwLock<HashMap<String, String>>,
    pub timelines: TokioRwLock<HashMap<String, Arc<AtomicI64>>>,
    pub preview_slots: TokioRwLock<HashMap<String, Arc<ArcSwapOption<RingBuffer>>>>,
    pub selection_lock: TokioMutex<()>,
    pub preview_lock: TokioMutex<()>,
    pub active: TokioRwLock<HashMap<String, Arc<ActiveIngest>>>,
    pub recent: TokioRwLock<HashMap<String, RecentIngestOutcome>>,
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
            selection_lock: TokioMutex::new(()),
            preview_lock: TokioMutex::new(()),
            active: TokioRwLock::new(HashMap::new()),
            recent: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct EgressRegistry {
    pub cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
    pub next_attempt_id: AtomicU64,
    pub active: TokioRwLock<HashMap<String, ActiveEgress>>,
    pub queues: TokioRwLock<HashMap<String, Arc<MemoryQueue>>>,
    pub recent: TokioRwLock<HashMap<String, RecentEgressOutcome>>,
    pub retry: TokioRwLock<HashMap<String, EgressRetryState>>,
}

impl Default for EgressRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EgressRegistry {
    pub fn new() -> Self {
        Self {
            cancel_tokens: TokioRwLock::new(HashMap::new()),
            next_attempt_id: AtomicU64::new(1),
            active: TokioRwLock::new(HashMap::new()),
            queues: TokioRwLock::new(HashMap::new()),
            recent: TokioRwLock::new(HashMap::new()),
            retry: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct RecordingRegistry {
    pub cancel_tokens: TokioRwLock<HashMap<String, CancellationToken>>,
}

impl Default for RecordingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingRegistry {
    pub fn new() -> Self {
        Self {
            cancel_tokens: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct HlsRegistry {
    pub stores: TokioRwLock<HashMap<String, Arc<HlsStore>>>,
    pub preview_stores: TokioRwLock<HashMap<String, Arc<Fmp4HlsStore>>>,
    pub consumers: TokioRwLock<HashMap<String, HlsConsumers>>,
}

impl Default for HlsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HlsRegistry {
    pub fn new() -> Self {
        Self {
            stores: TokioRwLock::new(HashMap::new()),
            preview_stores: TokioRwLock::new(HashMap::new()),
            consumers: TokioRwLock::new(HashMap::new()),
        }
    }
}

pub struct FileIngestRegistry {
    pub children: TokioRwLock<HashMap<String, tokio::process::Child>>,
    pub active: TokioRwLock<HashSet<String>>,
}

impl Default for FileIngestRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FileIngestRegistry {
    pub fn new() -> Self {
        Self {
            children: TokioRwLock::new(HashMap::new()),
            active: TokioRwLock::new(HashSet::new()),
        }
    }
}

#[derive(Clone)]
pub struct StageRuntime {
    pub ring: Option<Arc<RingBuffer>>,
    pub cancel: CancellationToken,
    pub lifecycle: Arc<StageLifecycle>,
    pub metrics: Arc<StageMetrics>,
    pub input_queue: Option<Arc<MemoryQueue>>,
    pub pipe_metrics: Option<Arc<PipeMetrics>>,
}

pub struct StageRegistry {
    pub runtimes: TokioRwLock<HashMap<StageKey, StageRuntime>>,
    pub metrics: TokioRwLock<HashMap<StageKey, Arc<StageMetrics>>>,
    pub ts_muxers: TokioRwLock<HashMap<String, Arc<TsChunkRing>>>,
    pub srt_muxer_shards: TokioRwLock<HashMap<String, SrtMuxerShardPool>>,
    pub lifecycles: TokioRwLock<HashMap<StageKey, Arc<StageLifecycle>>>,
}

impl Default for StageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl StageRegistry {
    pub fn new() -> Self {
        Self {
            runtimes: TokioRwLock::new(HashMap::new()),
            metrics: TokioRwLock::new(HashMap::new()),
            ts_muxers: TokioRwLock::new(HashMap::new()),
            srt_muxer_shards: TokioRwLock::new(HashMap::new()),
            lifecycles: TokioRwLock::new(HashMap::new()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrtMuxerAssignment {
    pub attempt_id: u64,
    pub shard_index: usize,
}

#[derive(Debug, Default)]
pub struct SrtMuxerShardPool {
    assignments: HashMap<String, SrtMuxerAssignment>,
    shard_occupancy: Vec<usize>,
    retiring_shards: HashSet<usize>,
    overflow_warned: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SrtMuxerAssignResult {
    pub shard_index: usize,
    pub shard_count: usize,
    pub shard_occupancy: usize,
    pub overflowed: bool,
    pub should_warn_overflow: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SrtMuxerReleaseResult {
    pub shard_index: usize,
    pub shard_empty: bool,
}

impl SrtMuxerShardPool {
    #[cfg(test)]
    pub(crate) fn test_snapshot(
        &self,
    ) -> (
        HashMap<String, SrtMuxerAssignment>,
        Vec<usize>,
        HashSet<usize>,
    ) {
        (
            self.assignments.clone(),
            self.shard_occupancy.clone(),
            self.retiring_shards.clone(),
        )
    }

    pub fn assign(
        &mut self,
        output_id: &str,
        attempt_id: u64,
        max_outputs_per_shard: usize,
        max_shards: usize,
    ) -> SrtMuxerAssignResult {
        debug_assert!(max_outputs_per_shard > 0);
        debug_assert!(max_shards > 0);

        if let Some(existing) = self.assignments.get(output_id).cloned() {
            if existing.attempt_id == attempt_id {
                let occupancy = self
                    .shard_occupancy
                    .get(existing.shard_index)
                    .copied()
                    .unwrap_or_default();
                return SrtMuxerAssignResult {
                    shard_index: existing.shard_index,
                    shard_count: self.shard_occupancy.len(),
                    shard_occupancy: occupancy,
                    overflowed: occupancy > max_outputs_per_shard,
                    should_warn_overflow: false,
                };
            }
            self.release_assignment(output_id, existing.attempt_id, false);
        }

        let mut overflowed = false;
        let shard_index = if let Some((index, _)) = self
            .shard_occupancy
            .iter()
            .enumerate()
            .filter(|(index, _)| !self.retiring_shards.contains(index))
            .filter(|(_, occupancy)| **occupancy < max_outputs_per_shard)
            .min_by_key(|(_, occupancy)| **occupancy)
        {
            index
        } else if self.shard_occupancy.len() < max_shards {
            self.shard_occupancy.push(0);
            self.shard_occupancy.len() - 1
        } else {
            overflowed = true;
            // Every existing shard is at (or over) max_outputs_per_shard, so
            // this is already an over-capacity fallback. Still exclude
            // retiring shards here: a retiring shard's backing muxer stage
            // may be concurrently torn down by a racing release, and handing
            // a new output that index would assign it a dead stage. If no
            // non-retiring shard exists, grow past max_shards rather than
            // reuse one.
            match self
                .shard_occupancy
                .iter()
                .enumerate()
                .filter(|(index, _)| !self.retiring_shards.contains(index))
                .min_by_key(|(_, occupancy)| **occupancy)
                .map(|(index, _)| index)
            {
                Some(index) => index,
                None => {
                    self.shard_occupancy.push(0);
                    self.shard_occupancy.len() - 1
                }
            }
        };

        self.shard_occupancy[shard_index] += 1;
        let shard_occupancy = self.shard_occupancy[shard_index];
        self.assignments.insert(
            output_id.to_string(),
            SrtMuxerAssignment {
                attempt_id,
                shard_index,
            },
        );

        let should_warn_overflow = overflowed && !self.overflow_warned;
        if overflowed {
            self.overflow_warned = true;
        }

        SrtMuxerAssignResult {
            shard_index,
            shard_count: self.shard_occupancy.len(),
            shard_occupancy,
            overflowed,
            should_warn_overflow,
        }
    }

    pub fn release(&mut self, output_id: &str, attempt_id: u64) -> Option<SrtMuxerReleaseResult> {
        self.release_assignment(output_id, attempt_id, true)
    }

    fn release_assignment(
        &mut self,
        output_id: &str,
        attempt_id: u64,
        retire_empty_shard: bool,
    ) -> Option<SrtMuxerReleaseResult> {
        let existing = self.assignments.get(output_id)?;
        if existing.attempt_id != attempt_id {
            return None;
        }
        let existing = self.assignments.remove(output_id)?;
        if let Some(occupancy) = self.shard_occupancy.get_mut(existing.shard_index) {
            *occupancy = occupancy.saturating_sub(1);
        }
        let shard_empty = self
            .shard_occupancy
            .get(existing.shard_index)
            .is_none_or(|occupancy| *occupancy == 0);
        if retire_empty_shard && shard_empty {
            self.retiring_shards.insert(existing.shard_index);
        }
        Some(SrtMuxerReleaseResult {
            shard_index: existing.shard_index,
            shard_empty,
        })
    }

    pub fn finish_retiring(&mut self, shard_index: usize) {
        self.retiring_shards.remove(&shard_index);
    }

    pub fn is_empty(&self) -> bool {
        self.assignments.is_empty()
    }
}

pub struct RuntimeInfra {
    pub listener_stats: Arc<ListenerSocketStats>,
    pub rtmp_listener_stats: Arc<RtmpListenerStats>,
    pub os_threads: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub listener_shutdowns: std::sync::Mutex<Vec<Box<dyn Fn() + Send + Sync>>>,
    pub sender_semaphore: Arc<tokio::sync::Semaphore>,
    pub srt_egress_muxer_port: Arc<Mutex<Option<u16>>>,
    pub external_ffmpeg_semaphore: Arc<tokio::sync::Semaphore>,
    pub diag_semaphores: TokioRwLock<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    pub event_log: Arc<EventLog>,
}

impl Default for RuntimeInfra {
    fn default() -> Self {
        Self::new(&crate::AppConfig::default())
    }
}

impl RuntimeInfra {
    pub fn new(config: &crate::AppConfig) -> Self {
        let external_ffmpeg_permits = config.external_ffmpeg_permits;
        Self {
            listener_stats: Arc::new(ListenerSocketStats::default()),
            rtmp_listener_stats: Arc::new(RtmpListenerStats::default()),
            os_threads: std::sync::Mutex::new(Vec::new()),
            listener_shutdowns: std::sync::Mutex::new(Vec::new()),
            sender_semaphore: Arc::new(tokio::sync::Semaphore::new(512)),
            srt_egress_muxer_port: Arc::new(Mutex::new(None)),
            external_ffmpeg_semaphore: Arc::new(tokio::sync::Semaphore::new(
                external_ffmpeg_permits,
            )),
            diag_semaphores: TokioRwLock::new(HashMap::new()),
            event_log: Arc::new(EventLog::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SrtMuxerShardPool;

    #[test]
    fn retiring_empty_srt_muxer_shard_is_not_reused_until_cleanup_finishes() {
        let mut pool = SrtMuxerShardPool::default();

        let first = pool.assign("out-1", 1, 1, 8);
        assert_eq!(first.shard_index, 0);

        let release = pool.release("out-1", 1).expect("assignment released");
        assert_eq!(release.shard_index, 0);
        assert!(release.shard_empty);

        let during_cleanup = pool.assign("out-2", 1, 1, 8);
        assert_eq!(during_cleanup.shard_index, 1);

        pool.finish_retiring(0);
        let after_cleanup = pool.assign("out-3", 1, 1, 8);
        assert_eq!(after_cleanup.shard_index, 0);
    }

    #[test]
    fn assign_overflow_does_not_reuse_a_still_retiring_shard() {
        // At max_shards capacity with the sole shard mid-teardown (retiring),
        // a new output must not land on that shard's index: its backing
        // muxer may be concurrently torn down by a racing release, so
        // reusing the index would hand the output a dead stage. The pool
        // must grow past max_shards instead of falling back to a retiring
        // shard.
        let mut pool = SrtMuxerShardPool::default();
        let first = pool.assign("out-1", 1, 1, 1);
        assert_eq!(first.shard_index, 0);

        let release = pool.release("out-1", 1).expect("assignment released");
        assert_eq!(release.shard_index, 0);
        assert!(release.shard_empty);

        let second = pool.assign("out-2", 1, 1, 1);
        assert_ne!(
            second.shard_index, 0,
            "out-2 must not be assigned to shard 0 while it is still retiring"
        );

        let (_, occupancy, retiring) = pool.test_snapshot();
        assert!(retiring.contains(&0));
        assert_eq!(occupancy[0], 0);
    }

    #[test]
    fn assign_same_output_and_attempt_is_idempotent_and_does_not_double_occupy() {
        let mut pool = SrtMuxerShardPool::default();

        let first = pool.assign("out-1", 1, 4, 8);
        let second = pool.assign("out-1", 1, 4, 8);

        assert_eq!(first.shard_index, second.shard_index);
        // Re-asserting the same (output, attempt) pair must not consume a
        // second occupancy slot on the shard.
        let (_, occupancy, _) = pool.test_snapshot();
        assert_eq!(occupancy[first.shard_index], 1);
        assert_eq!(second.shard_occupancy, 1);
    }

    #[test]
    fn assign_overflow_boundary_uses_strict_greater_than_on_idempotent_path() {
        // Fill a single shard to exactly max_outputs_per_shard, then
        // re-assert the same (output, attempt) pair already on that shard.
        // The idempotent-return branch reports `overflowed` only when
        // occupancy is strictly greater than capacity, so sitting exactly
        // at capacity must not be flagged as overflowed.
        let mut pool = SrtMuxerShardPool::default();
        pool.assign("out-1", 1, 1, 1);

        let reassert = pool.assign("out-1", 1, 1, 1);
        assert_eq!(reassert.shard_occupancy, 1);
        assert!(!reassert.overflowed);
    }

    #[test]
    fn assign_reconnect_with_new_attempt_id_frees_old_shard_immediately() {
        // A publisher reconnect reuses the same output_id but gets a new
        // attempt_id; the stale assignment must be released (not leaked)
        // and the freed shard must be immediately reusable without going
        // through the retiring-shard cleanup path.
        let mut pool = SrtMuxerShardPool::default();
        let first = pool.assign("out-1", 1, 1, 1);
        assert_eq!(first.shard_index, 0);

        let reconnected = pool.assign("out-1", 2, 1, 1);
        assert_eq!(reconnected.shard_index, 0);
        assert_eq!(reconnected.shard_occupancy, 1);

        let (assignments, occupancy, retiring) = pool.test_snapshot();
        assert_eq!(assignments.get("out-1").unwrap().attempt_id, 2);
        assert_eq!(occupancy[0], 1);
        assert!(retiring.is_empty());
    }

    #[test]
    fn assign_least_occupied_shard_selection_prefers_first_tied_minimum() {
        let mut pool = SrtMuxerShardPool::default();
        let a = pool.assign("out-1", 1, 1, 3);
        let b = pool.assign("out-2", 1, 1, 3);
        let c = pool.assign("out-3", 1, 1, 3);

        assert_eq!((a.shard_index, b.shard_index, c.shard_index), (0, 1, 2));
    }

    #[test]
    fn assign_overflow_warns_only_once_when_shards_and_capacity_are_exhausted() {
        let mut pool = SrtMuxerShardPool::default();
        let first = pool.assign("out-1", 1, 1, 1);
        assert!(!first.overflowed);

        let second = pool.assign("out-2", 1, 1, 1);
        assert!(second.overflowed);
        assert!(second.should_warn_overflow);

        let third = pool.assign("out-3", 1, 1, 1);
        assert!(third.overflowed);
        assert!(!third.should_warn_overflow);
    }

    #[test]
    fn release_with_mismatched_attempt_id_is_a_stale_noop() {
        // A cleanup task from a superseded attempt racing after a
        // reconnect must not be able to release the current assignment.
        let mut pool = SrtMuxerShardPool::default();
        pool.assign("out-1", 1, 4, 8);

        assert_eq!(pool.release("out-1", 999), None);

        let (assignments, occupancy, _) = pool.test_snapshot();
        assert_eq!(assignments.get("out-1").unwrap().attempt_id, 1);
        assert_eq!(occupancy[0], 1);
    }

    #[test]
    fn release_unknown_output_returns_none_without_panicking() {
        let mut pool = SrtMuxerShardPool::default();
        assert_eq!(pool.release("never-assigned", 1), None);
    }

    #[test]
    fn finish_retiring_unknown_shard_is_a_noop() {
        let mut pool = SrtMuxerShardPool::default();
        // Must not panic even though shard 7 was never allocated, let
        // alone marked retiring.
        pool.finish_retiring(7);
        assert!(pool.is_empty());
    }

    #[test]
    #[should_panic]
    fn assign_panics_on_zero_max_shards_invariant() {
        let mut pool = SrtMuxerShardPool::default();
        pool.assign("out-1", 1, 4, 0);
    }

    #[test]
    #[should_panic]
    fn assign_panics_on_zero_max_outputs_per_shard_invariant() {
        let mut pool = SrtMuxerShardPool::default();
        pool.assign("out-1", 1, 0, 4);
    }
}
