//! Registry structs for `MediaEngine` state ownership, grouping the
//! synchronized maps and sets that back ingest, egress, HLS, and stage lifecycles.
//!
//! Registry access rule: do not hold registry guards across awaits that acquire
//! another registry. Snapshot paths should copy or render the fields they need
//! inside a bounded scope, drop all guards, then perform async enrichment such
//! as stage or HLS lookups. Lifecycle paths that must touch more than one
//! registry keep the critical section short and avoid calling out while guards
//! are live.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock as TokioRwLock;
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
    pub active: TokioRwLock<HashMap<String, ActiveIngest>>,
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
            self.shard_occupancy
                .iter()
                .enumerate()
                .filter(|(index, _)| !self.retiring_shards.contains(index))
                .min_by_key(|(_, occupancy)| **occupancy)
                .or_else(|| {
                    self.shard_occupancy
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, occupancy)| **occupancy)
                })
                .map(|(index, _)| index)
                .unwrap_or_else(|| {
                    self.shard_occupancy.push(0);
                    0
                })
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
}
