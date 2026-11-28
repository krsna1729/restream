//! Shared-stage state owned by `MediaEngine`: the stage runtimes, their
//! side-map metrics and lifecycles, the shared TS muxers, the SRT egress
//! muxer shard pools, and the consumer leases that keep a stage alive; the
//! operations over that state.
//!
//! Every consumer of a shared stage (an egress, an HLS preview, an SRT
//! player) holds a [`StageLease`] on each stage it reads. The registry entry
//! keeps only a `Weak` to the lease cell, so the stage lives exactly as long
//! as its consumers: dropping the last lease cancels the stage and removes
//! its entry. No sweep re-derives use from the outputs, and no grace period
//! covers consumers that have not announced themselves yet. The lease and the
//! registry refer to each other (`StageRuntime` -> `Weak<LeaseCell>` ->
//! `Weak<StageRegistry>`), so both live in this one module.
//!
//! The registry access rules in the `engine_registries` module docs apply.
//! The few sections that must hold more than one stage guard take them in
//! this order, and never the reverse:
//!
//! `runtimes` -> `metrics` -> `lifecycles`.
//!
//! The orders that span registries are in the `engine_registries` module
//! docs.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use restream_domain::domain::stage::StageKey;
use restream_domain::runtime::stage::StageRuntimeSnapshot;
use restream_sync::RingBuffer;
use tokio::sync::RwLock as TokioRwLock;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::pipe_metrics::PipeMetrics;
use crate::stage_lifecycle::{
    StageBackendKind, StageLifecycle, StageLifecycleSnapshot, StagePhase,
};
use crate::stage_metrics::StageMetrics;
use crate::ts_chunk_ring::TsChunkRing;

#[derive(Clone)]
pub struct StageRuntime {
    pub ring: Option<Arc<RingBuffer>>,
    pub cancel: CancellationToken,
    pub lifecycle: Arc<StageLifecycle>,
    pub metrics: Arc<StageMetrics>,
    pub pipe_metrics: Option<Arc<PipeMetrics>>,
    /// Consumers' leases (`Weak::new()` for stages no consumer leases).
    pub lease: Weak<LeaseCell>,
    /// `StageLease::id` of `lease`; 0 when unleased.
    pub lease_id: u64,
}

/// A shared TS muxer and its consumers' leases.
pub struct TsMuxerStage {
    pub muxer: Arc<TsChunkRing>,
    pub lease: Weak<LeaseCell>,
    /// `StageLease::id` of `lease`.
    pub lease_id: u64,
}

pub struct StageRegistry {
    pub runtimes: TokioRwLock<HashMap<StageKey, StageRuntime>>,
    pub metrics: TokioRwLock<HashMap<StageKey, Arc<StageMetrics>>>,
    pub ts_muxers: TokioRwLock<HashMap<String, TsMuxerStage>>,
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

    /// Runs `f` over every stage runtime; `f` runs under the runtime map's
    /// read guard and must not wait.
    pub async fn with_runtimes<R>(
        &self,
        f: impl FnOnce(&HashMap<StageKey, StageRuntime>) -> R,
    ) -> R {
        f(&*self.runtimes.read().await)
    }

    /// Runs `f` over the side-map metrics (stages without a runtime); `f`
    /// runs under the metrics map's read guard and must not wait.
    pub async fn with_metrics<R>(
        &self,
        f: impl FnOnce(&HashMap<StageKey, Arc<StageMetrics>>) -> R,
    ) -> R {
        f(&*self.metrics.read().await)
    }

    /// Runs `f` over every shared TS muxer, keyed by `<pipeline>:<stage>`;
    /// `f` runs under the muxer map's read guard and must not wait.
    pub async fn with_ts_muxers<R>(
        &self,
        f: impl FnOnce(&HashMap<String, TsMuxerStage>) -> R,
    ) -> R {
        f(&*self.ts_muxers.read().await)
    }

    pub async fn register_pipe_metrics(&self, key: StageKey, metrics: Arc<PipeMetrics>) {
        if let Some(runtime) = self.runtimes.write().await.get_mut(&key) {
            runtime.pipe_metrics = Some(metrics.clone());
        }
    }

    pub async fn remove_pipe_metrics(&self, key: &StageKey) {
        if let Some(runtime) = self.runtimes.write().await.get_mut(key) {
            runtime.pipe_metrics = None;
        }
    }

    pub async fn get_or_create_non_ring_stage_runtime(
        &self,
        key: StageKey,
        initial: StagePhase,
        backend: StageBackendKind,
        cancel: CancellationToken,
    ) -> (Arc<StageLifecycle>, Arc<StageMetrics>) {
        let mut runtimes = self.runtimes.write().await;
        if let Some(runtime) = runtimes.get(&key)
            && !runtime.cancel.is_cancelled()
        {
            return (runtime.lifecycle.clone(), runtime.metrics.clone());
        }

        let lifecycle = Arc::new(StageLifecycle::new_with_backend(initial, backend));
        let metrics = Arc::new(StageMetrics::new());
        runtimes.insert(
            key,
            StageRuntime {
                ring: None,
                cancel,
                lifecycle: lifecycle.clone(),
                metrics: metrics.clone(),
                pipe_metrics: None,
                lease: Weak::new(),
                lease_id: 0,
            },
        );
        (lifecycle, metrics)
    }

    pub async fn get_or_create_stage_metrics(&self, key: StageKey) -> Arc<StageMetrics> {
        if let Some(runtime) = self.runtimes.read().await.get(&key).cloned() {
            return runtime.metrics;
        }
        let mut metrics = self.metrics.write().await;
        metrics
            .entry(key)
            .or_insert_with(|| Arc::new(StageMetrics::new()))
            .clone()
    }

    pub async fn remove_stage_metrics(&self, key: &StageKey) {
        self.metrics.write().await.remove(key);
    }

    pub async fn remove_stage_runtime(&self, key: &StageKey) {
        self.runtimes.write().await.remove(key);
    }

    /// Detached teardown must not remove a newer runtime registered at this key.
    pub(crate) async fn remove_stage_runtime_if_current(
        &self,
        key: &StageKey,
        lifecycle: &Arc<StageLifecycle>,
    ) {
        let mut runtimes = self.runtimes.write().await;
        if runtimes
            .get(key)
            .is_some_and(|runtime| Arc::ptr_eq(&runtime.lifecycle, lifecycle))
        {
            runtimes.remove(key);
        }
    }

    /// The live shared TS muxer under `key`, and the caller's lease on it:
    /// the muxer stops when its last lease drops. Without a live one, `start`
    /// starts a muxer under the cancel token the new lease holds; it runs
    /// under the muxer map's write guard and must not wait.
    pub(crate) async fn get_or_create_ts_muxer(
        self: &Arc<Self>,
        key: String,
        start: impl FnOnce(CancellationToken) -> Arc<TsChunkRing>,
    ) -> (Arc<TsChunkRing>, StageLease) {
        let mut stages = self.ts_muxers.write().await;
        if let Some(stage) = stages.get(&key)
            && !stage.muxer.cancel.is_cancelled()
            && let Some(lease) = StageLease::upgrade(&stage.lease)
        {
            return (stage.muxer.clone(), lease);
        }

        let cancel = CancellationToken::new();
        let lease = StageLease::ts_muxer(self, key.clone(), cancel.clone());
        let shared_muxer = start(cancel);

        stages.insert(
            key,
            TsMuxerStage {
                muxer: shared_muxer.clone(),
                lease: lease.downgrade(),
                lease_id: lease.id(),
            },
        );
        (shared_muxer, lease)
    }

    /// Last lease of a TS muxer dropped (its token is already cancelled).
    async fn release_ts_muxer_stage(&self, key: &str, lease_id: u64) {
        let mut stages = self.ts_muxers.write().await;
        if stages
            .get(key)
            .is_some_and(|stage| stage.lease_id == lease_id)
        {
            stages.remove(key);
        }
    }

    /// Last lease of a stage runtime dropped (its token is already cancelled).
    #[expect(
        clippy::await_holding_invalid_type,
        reason = "lock order `runtimes` -> `metrics` -> `lifecycles` (module docs of `stage_registry`): held so a concurrent `ensure_stage` cannot adopt these (below)"
    )]
    async fn release_stage_runtime(&self, key: &StageKey, lease_id: u64) {
        let mut runtimes = self.runtimes.write().await;
        if runtimes
            .get(key)
            .is_none_or(|runtime| runtime.lease_id != lease_id)
        {
            return;
        }
        runtimes.remove(key);
        // Under the runtimes lock (order: runtimes, then metrics, then
        // lifecycles), so a concurrent ensure_stage cannot adopt these.
        self.metrics.write().await.remove(key);
        self.lifecycles.write().await.remove(key);
        drop(runtimes);
    }

    fn srt_muxer_cohort_key(pipeline_id: &str, encoding: &str) -> String {
        format!("{pipeline_id}\u{1f}{encoding}")
    }

    fn srt_muxer_stage_key(encoding: &str, shard_index: usize) -> String {
        format!("{encoding}:srt-mux-shard:{shard_index}")
    }

    /// The TS muxer stage key an SRT egress output reads: `encoding` itself
    /// when sharding is off (`max_outputs_per_shard` 0), else its shard's.
    pub async fn assign_srt_egress_muxer_stage(
        &self,
        pipeline_id: &str,
        encoding: &str,
        output_id: &str,
        attempt_id: u64,
        max_outputs_per_shard: usize,
        max_shards: usize,
    ) -> String {
        if max_outputs_per_shard == 0 {
            return encoding.to_string();
        }

        let max_shards = max_shards.max(1);
        let cohort_key = Self::srt_muxer_cohort_key(pipeline_id, encoding);
        let mut pools = self.srt_muxer_shards.write().await;
        let result = pools.entry(cohort_key.clone()).or_default().assign(
            output_id,
            attempt_id,
            max_outputs_per_shard,
            max_shards,
        );
        drop(pools);

        if result.should_warn_overflow {
            warn!(
                pipeline_id,
                encoding,
                cohort_key,
                max_outputs_per_shard,
                max_shards,
                shard_count = result.shard_count,
                shard_index = result.shard_index,
                shard_occupancy = result.shard_occupancy,
                "SRT muxer shard cap exceeded; additional outputs are sharing existing muxers"
            );
        }

        Self::srt_muxer_stage_key(encoding, result.shard_index)
    }

    /// Releases an SRT egress output's muxer shard assignment; a shard left
    /// empty has its muxer stage removed. A no-op when sharding is off.
    pub async fn release_srt_egress_muxer_stage(
        &self,
        pipeline_id: &str,
        encoding: &str,
        output_id: &str,
        attempt_id: u64,
        max_outputs_per_shard: usize,
    ) {
        if max_outputs_per_shard == 0 {
            return;
        }

        let cohort_key = Self::srt_muxer_cohort_key(pipeline_id, encoding);
        let result = {
            let mut pools = self.srt_muxer_shards.write().await;
            let Some(pool) = pools.get_mut(&cohort_key) else {
                return;
            };
            let Some(result) = pool.release(output_id, attempt_id) else {
                return;
            };
            result
        };

        if result.shard_empty {
            let stage_key = Self::srt_muxer_stage_key(encoding, result.shard_index);
            self.remove_ts_muxer_stage(pipeline_id, &stage_key).await;

            let mut pools = self.srt_muxer_shards.write().await;
            if let Some(pool) = pools.get_mut(&cohort_key) {
                pool.finish_retiring(result.shard_index);
                if pool.is_empty() {
                    pools.remove(&cohort_key);
                }
            }
        }
    }

    async fn remove_ts_muxer_stage(&self, pipeline_id: &str, stage_key: &str) {
        let key = format!("{}:{}", pipeline_id, stage_key);
        let mut stages = self.ts_muxers.write().await;
        if let Some(stage) = stages.remove(&key) {
            stage.muxer.cancel.cancel();
        }
    }

    pub async fn get_or_create_stage_lifecycle(
        &self,
        key: StageKey,
        initial: StagePhase,
    ) -> Arc<StageLifecycle> {
        if let Some(runtime) = self.runtimes.read().await.get(&key).cloned() {
            return runtime.lifecycle;
        }
        let mut lifecycles = self.lifecycles.write().await;
        lifecycles
            .entry(key)
            .or_insert_with(|| Arc::new(StageLifecycle::new(initial)))
            .clone()
    }

    pub async fn get_or_create_stage_lifecycle_with_backend(
        &self,
        key: StageKey,
        initial: StagePhase,
        backend: StageBackendKind,
    ) -> Arc<StageLifecycle> {
        if let Some(runtime) = self.runtimes.read().await.get(&key).cloned() {
            return runtime.lifecycle;
        }
        let mut lifecycles = self.lifecycles.write().await;
        lifecycles
            .entry(key)
            .or_insert_with(|| Arc::new(StageLifecycle::new_with_backend(initial, backend)))
            .clone()
    }

    pub async fn remove_stage_lifecycle(&self, key: &StageKey) {
        self.lifecycles.write().await.remove(key);
    }

    pub async fn stage_lifecycle_snapshot(&self, key: &StageKey) -> Option<StageLifecycleSnapshot> {
        if let Some(runtime) = self.runtimes.read().await.get(key).cloned() {
            return Some(runtime.lifecycle.snapshot());
        }
        self.lifecycles
            .read()
            .await
            .get(key)
            .map(|lc| lc.snapshot())
    }

    /// A stage's lifecycle snapshot and metrics: its runtime's, else the side
    /// maps'.
    pub(crate) async fn stage_snapshot_parts(
        &self,
        key: &StageKey,
    ) -> Option<(StageLifecycleSnapshot, Arc<StageMetrics>)> {
        if let Some(runtime) = self.runtimes.read().await.get(key).cloned() {
            return Some((runtime.lifecycle.snapshot(), runtime.metrics.clone()));
        }
        let lifecycle = self.stage_lifecycle_snapshot(key).await?;
        let metrics = self
            .metrics
            .read()
            .await
            .get(key)
            .cloned()
            .unwrap_or_else(|| Arc::new(StageMetrics::new()));
        Some((lifecycle, metrics))
    }

    /// A stage's runtime snapshot (see [`stage_runtime_snapshot`]).
    pub(crate) async fn stage_runtime_snapshot(
        &self,
        key: &StageKey,
        ffmpeg_permits: &Semaphore,
        ffmpeg_permits_total: usize,
    ) -> Option<StageRuntimeSnapshot> {
        let (lifecycle, metrics) = self.stage_snapshot_parts(key).await?;
        Some(stage_runtime_snapshot(
            key,
            &lifecycle,
            &metrics,
            ffmpeg_permits,
            ffmpeg_permits_total,
        ))
    }

    /// The keys of every stage of `pipeline_id` with a runtime or a side-map
    /// lifecycle, one map at a time.
    pub(crate) async fn pipeline_stage_keys(&self, pipeline_id: &str) -> HashSet<StageKey> {
        let mut keys = HashSet::new();
        {
            let runtimes = self.runtimes.read().await;
            keys.extend(
                runtimes
                    .keys()
                    .filter(|k| k.pipeline.as_str() == pipeline_id)
                    .cloned(),
            );
        }
        {
            let lifecycles = self.lifecycles.read().await;
            keys.extend(
                lifecycles
                    .keys()
                    .filter(|k| k.pipeline.as_str() == pipeline_id)
                    .cloned(),
            );
        }
        keys
    }
}

/// A stage's runtime snapshot from its lifecycle and metrics. While the stage
/// waits for or holds an external FFmpeg permit, it reports the permits
/// (`ffmpeg_permits`, of `ffmpeg_permits_total`) and how long it has waited.
pub(crate) fn stage_runtime_snapshot(
    key: &StageKey,
    lifecycle: &StageLifecycleSnapshot,
    metrics: &StageMetrics,
    ffmpeg_permits: &Semaphore,
    ffmpeg_permits_total: usize,
) -> StageRuntimeSnapshot {
    let (capacity_permits_total, capacity_permits_available, capacity_wait_ms) = if matches!(
        lifecycle.phase,
        StagePhase::WaitingForCapacity { .. } | StagePhase::CapacityAcquired { .. }
    ) {
        let total = Some(ffmpeg_permits_total);
        let available = Some(ffmpeg_permits.available_permits());
        let wait_ms = lifecycle
            .phase_started_at
            .map(|t| std::cmp::min(t.elapsed().as_millis(), u64::MAX as u128) as u64);
        (total, available, wait_ms)
    } else {
        (None, None, None)
    };

    StageRuntimeSnapshot {
        key: key.clone(),
        backend: lifecycle.backend,
        phase: lifecycle.phase.clone(),
        backend_pid: lifecycle.backend_pid,
        bytes_in: metrics.bytes_in.load(Ordering::Relaxed),
        bytes_out: metrics.bytes_out.load(Ordering::Relaxed),
        packets_in: metrics.packets_in.load(Ordering::Relaxed),
        packets_out: metrics.packets_out.load(Ordering::Relaxed),
        first_input_at: lifecycle.first_input_at,
        first_output_at: lifecycle.first_output_at,
        last_error: lifecycle.last_error.clone(),
        capacity_permits_total,
        capacity_permits_available,
        capacity_wait_ms,
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

/// One consumer's hold on a shared stage. Clones share the hold.
#[derive(Clone)]
pub struct StageLease(#[allow(dead_code)] Arc<LeaseCell>);

pub struct LeaseCell {
    /// Unique for the process: the registry entry records it, so a release
    /// removes only the entry this cell created (an address can be reused).
    id: u64,
    registry: Weak<StageRegistry>,
    /// Captured at creation (always on Tokio): the last lease can drop on a
    /// Compio Owner thread (an SRT player), and the release runs on Tokio.
    runtime: Option<tokio::runtime::Handle>,
    stage: LeasedStage,
    cancel: CancellationToken,
}

enum LeasedStage {
    Runtime(StageKey),
    TsMuxer(String),
}

impl StageLease {
    pub(crate) fn runtime(
        registry: &Arc<StageRegistry>,
        key: StageKey,
        cancel: CancellationToken,
    ) -> Self {
        Self::new(registry, LeasedStage::Runtime(key), cancel)
    }

    fn ts_muxer(registry: &Arc<StageRegistry>, key: String, cancel: CancellationToken) -> Self {
        Self::new(registry, LeasedStage::TsMuxer(key), cancel)
    }

    fn new(registry: &Arc<StageRegistry>, stage: LeasedStage, cancel: CancellationToken) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self(Arc::new(LeaseCell {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            registry: Arc::downgrade(registry),
            runtime: tokio::runtime::Handle::try_current().ok(),
            stage,
            cancel,
        }))
    }

    /// The registry's reference: upgrades while any consumer holds the lease.
    pub(crate) fn downgrade(&self) -> Weak<LeaseCell> {
        Arc::downgrade(&self.0)
    }

    /// The id the registry entry records for this lease (never 0).
    pub(crate) fn id(&self) -> u64 {
        self.0.id
    }

    /// Another hold on a live stage, if any consumer still holds one.
    pub(crate) fn upgrade(weak: &Weak<LeaseCell>) -> Option<Self> {
        weak.upgrade().map(Self)
    }

    /// Completes once the leased stage is cancelled. While this lease lives
    /// that means the stage ended on its own (it is never reused).
    pub(crate) fn ended(&self) -> tokio_util::sync::WaitForCancellationFuture<'_> {
        self.0.cancel.cancelled()
    }

    /// Completes once any of `stages` ends on its own; never, for none.
    pub async fn any_ended(stages: &[StageLease]) {
        if stages.is_empty() {
            return std::future::pending().await;
        }
        futures_util::future::select_all(stages.iter().map(|stage| Box::pin(stage.ended()))).await;
    }
}

impl Drop for LeaseCell {
    fn drop(&mut self) {
        self.cancel.cancel();
        let (Some(registry), Some(runtime)) = (self.registry.upgrade(), self.runtime.as_ref())
        else {
            // A cancelled entry is never reused; the next consumer replaces it.
            return;
        };
        // A newer stage under the same key records another id and is left
        // alone, even if its cell reuses this allocation.
        let id = self.id;
        let stage = match &self.stage {
            LeasedStage::Runtime(key) => LeasedStage::Runtime(key.clone()),
            LeasedStage::TsMuxer(key) => LeasedStage::TsMuxer(key.clone()),
        };
        runtime.spawn(async move {
            match stage {
                LeasedStage::Runtime(key) => registry.release_stage_runtime(&key, id).await,
                LeasedStage::TsMuxer(key) => registry.release_ts_muxer_stage(&key, id).await,
            }
        });
    }
}

#[cfg(test)]
mod tests;
