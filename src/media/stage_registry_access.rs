use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::domain::stage::{StageKey, StageKind};
use crate::media::avio::MemoryQueue;
use crate::media::engine::{ActiveEgress, MediaEngine, hls_preview_registry_key};
use crate::media::engine_hls::input_id_from_hls_preview_resource_id;
use crate::media::engine_registries::StageRuntime;
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_lifecycle::{
    StageBackendKind, StageLifecycle, StageLifecycleSnapshot, StagePhase,
};
use crate::media::stage_metrics::StageMetrics;
use crate::media::ts_chunk_ring::TsChunkRing;
use crate::runtime::stage::StageRuntimeSnapshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;

impl MediaEngine {
    pub async fn register_input_queue(&self, key: StageKey, queue: Arc<MemoryQueue>) {
        if let Some(runtime) = self.stages.runtimes.write().await.get_mut(&key) {
            runtime.input_queue = Some(queue.clone());
        }
    }

    pub async fn remove_input_queue(&self, key: &StageKey) {
        if let Some(runtime) = self.stages.runtimes.write().await.get_mut(key) {
            runtime.input_queue = None;
        }
    }

    pub async fn register_pipe_metrics(&self, key: StageKey, metrics: Arc<PipeMetrics>) {
        if let Some(runtime) = self.stages.runtimes.write().await.get_mut(&key) {
            runtime.pipe_metrics = Some(metrics.clone());
        }
    }

    pub async fn remove_pipe_metrics(&self, key: &StageKey) {
        if let Some(runtime) = self.stages.runtimes.write().await.get_mut(key) {
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
        let mut runtimes = self.stages.runtimes.write().await;
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
                input_queue: None,
                pipe_metrics: None,
            },
        );
        (lifecycle, metrics)
    }

    pub async fn get_or_create_stage_metrics(&self, key: StageKey) -> Arc<StageMetrics> {
        if let Some(runtime) = self.stages.runtimes.read().await.get(&key).cloned() {
            return runtime.metrics;
        }
        let mut metrics = self.stages.metrics.write().await;
        metrics
            .entry(key)
            .or_insert_with(|| Arc::new(StageMetrics::new()))
            .clone()
    }

    pub async fn remove_stage_metrics(&self, key: &StageKey) {
        self.stages.metrics.write().await.remove(key);
    }

    pub async fn remove_stage_runtime(&self, key: &StageKey) {
        self.stages.runtimes.write().await.remove(key);
    }

    pub async fn get_or_create_ts_muxer_stage(
        self: &Arc<Self>,
        pipeline_id: &str,
        stage_key: &str,
        source_ring: Arc<RingBuffer>,
    ) -> Arc<TsChunkRing> {
        let key = format!("{}:{}", pipeline_id, stage_key);

        let mut stages = self.stages.ts_muxers.write().await;
        if let Some(stage) = stages.get(&key)
            && !stage.cancel.is_cancelled()
        {
            return stage.clone();
        }

        let cancel = CancellationToken::new();
        let shared_muxer = crate::media::srt::start_shared_ts_muxer(
            pipeline_id,
            stage_key,
            source_ring,
            self.clone(),
            cancel,
        );

        stages.insert(key, shared_muxer.clone());
        shared_muxer
    }

    fn srt_muxer_cohort_key(pipeline_id: &str, encoding: &str) -> String {
        format!("{pipeline_id}\u{1f}{encoding}")
    }

    fn srt_muxer_stage_key(encoding: &str, shard_index: usize) -> String {
        format!("{encoding}:srt-mux-shard:{shard_index}")
    }

    pub async fn assign_srt_egress_muxer_stage(
        &self,
        pipeline_id: &str,
        encoding: &str,
        output_id: &str,
        attempt_id: u64,
    ) -> String {
        let max_outputs_per_shard = self.config.srt_egress_muxer_max_outputs_per_shard;
        if max_outputs_per_shard == 0 {
            return encoding.to_string();
        }

        let max_shards = self.config.srt_egress_muxer_max_shards.max(1);
        let cohort_key = Self::srt_muxer_cohort_key(pipeline_id, encoding);
        let mut pools = self.stages.srt_muxer_shards.write().await;
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

    pub async fn release_srt_egress_muxer_stage(
        &self,
        pipeline_id: &str,
        encoding: &str,
        output_id: &str,
        attempt_id: u64,
    ) {
        if self.config.srt_egress_muxer_max_outputs_per_shard == 0 {
            return;
        }

        let cohort_key = Self::srt_muxer_cohort_key(pipeline_id, encoding);
        let mut pools = self.stages.srt_muxer_shards.write().await;
        let Some(pool) = pools.get_mut(&cohort_key) else {
            return;
        };
        let Some(result) = pool.release(output_id, attempt_id) else {
            return;
        };
        drop(pools);

        if result.shard_empty {
            let stage_key = Self::srt_muxer_stage_key(encoding, result.shard_index);
            self.remove_ts_muxer_stage(pipeline_id, &stage_key).await;

            let mut pools = self.stages.srt_muxer_shards.write().await;
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
        let mut stages = self.stages.ts_muxers.write().await;
        if let Some(stage) = stages.remove(&key) {
            stage.cancel.cancel();
        }
    }

    pub async fn get_or_create_stage_lifecycle(
        &self,
        key: StageKey,
        initial: StagePhase,
    ) -> Arc<StageLifecycle> {
        if let Some(runtime) = self.stages.runtimes.read().await.get(&key).cloned() {
            return runtime.lifecycle;
        }
        let mut lifecycles = self.stages.lifecycles.write().await;
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
        if let Some(runtime) = self.stages.runtimes.read().await.get(&key).cloned() {
            return runtime.lifecycle;
        }
        let mut lifecycles = self.stages.lifecycles.write().await;
        lifecycles
            .entry(key)
            .or_insert_with(|| Arc::new(StageLifecycle::new_with_backend(initial, backend)))
            .clone()
    }

    pub async fn remove_stage_lifecycle(&self, key: &StageKey) {
        self.stages.lifecycles.write().await.remove(key);
    }

    pub async fn stage_lifecycle_snapshot(&self, key: &StageKey) -> Option<StageLifecycleSnapshot> {
        if let Some(runtime) = self.stages.runtimes.read().await.get(key).cloned() {
            return Some(runtime.lifecycle.snapshot());
        }
        self.stages
            .lifecycles
            .read()
            .await
            .get(key)
            .map(|lc| lc.snapshot())
    }

    async fn stage_snapshot_parts(
        &self,
        key: &StageKey,
    ) -> Option<(StageLifecycleSnapshot, Arc<StageMetrics>)> {
        if let Some(runtime) = self.stages.runtimes.read().await.get(key).cloned() {
            return Some((runtime.lifecycle.snapshot(), runtime.metrics.clone()));
        }
        let lifecycle = self.stage_lifecycle_snapshot(key).await?;
        let metrics = self
            .stages
            .metrics
            .read()
            .await
            .get(key)
            .cloned()
            .unwrap_or_else(|| Arc::new(StageMetrics::new()));
        Some((lifecycle, metrics))
    }

    fn build_stage_runtime_snapshot(
        &self,
        key: &StageKey,
        lifecycle: StageLifecycleSnapshot,
        metrics: Arc<StageMetrics>,
    ) -> StageRuntimeSnapshot {
        let (capacity_permits_total, capacity_permits_available, capacity_wait_ms) = if matches!(
            lifecycle.phase,
            StagePhase::WaitingForCapacity { .. } | StagePhase::CapacityAcquired { .. }
        ) {
            let semaphore = &self.runtime.external_ffmpeg_semaphore;
            let total = Some(self.config.external_ffmpeg_permits);
            let available = Some(semaphore.available_permits());
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

    pub async fn stage_runtime_snapshot(&self, key: &StageKey) -> Option<StageRuntimeSnapshot> {
        let (lifecycle, metrics) = self.stage_snapshot_parts(key).await?;
        Some(self.build_stage_runtime_snapshot(key, lifecycle, metrics))
    }

    /// Return runtime snapshots for all stages belonging to the given pipeline.
    pub async fn pipeline_stage_runtime_snapshots(
        &self,
        pipeline_id: &str,
    ) -> Vec<StageRuntimeSnapshot> {
        let mut keys = HashSet::new();
        {
            let runtimes = self.stages.runtimes.read().await;
            keys.extend(
                runtimes
                    .keys()
                    .filter(|k| k.pipeline.as_str() == pipeline_id)
                    .cloned(),
            );
        }
        {
            let lifecycles = self.stages.lifecycles.read().await;
            keys.extend(
                lifecycles
                    .keys()
                    .filter(|k| k.pipeline.as_str() == pipeline_id)
                    .cloned(),
            );
        }

        let mut snapshots = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(snap) = self.stage_runtime_snapshot(&key).await {
                snapshots.push(snap);
            }
        }
        snapshots
    }

    /// Returns the blocking upstream stage snapshot for an egress when its
    /// terminal stage is not yet producing. `None` means the stage is healthy,
    /// unknown, or already producing.
    pub async fn egress_blocked_by_snapshot(
        &self,
        egress: &ActiveEgress,
    ) -> Option<StageRuntimeSnapshot> {
        let key = egress.terminal_stage_key.as_ref()?;
        self.egress_blocked_by_stage_snapshot(key).await
    }

    /// Key-based variant of `egress_blocked_by_snapshot` for callers that
    /// need to drop egress registry guards before awaiting stage registries.
    pub async fn egress_blocked_by_stage_snapshot(
        &self,
        key: &StageKey,
    ) -> Option<StageRuntimeSnapshot> {
        let (lifecycle, metrics) = self.stage_snapshot_parts(key).await?;
        if matches!(lifecycle.phase, StagePhase::Producing) {
            return None;
        }
        Some(self.build_stage_runtime_snapshot(key, lifecycle, metrics))
    }

    /// Returns the blocking upstream stage snapshot for an HLS preview when
    /// any of its active preview/transcode stages are not yet producing.
    pub async fn preview_blocked_by_snapshot(
        &self,
        pipeline_id: &str,
    ) -> Option<StageRuntimeSnapshot> {
        let mut keys: Vec<_> = self
            .active_hls_preview_stage_keys()
            .await
            .into_iter()
            .filter(|key| key.pipeline.as_str() == pipeline_id)
            .collect();
        keys.sort_by_key(|key| key.to_string());

        for key in keys {
            let Some((lifecycle, metrics)) = self.stage_snapshot_parts(&key).await else {
                continue;
            };
            if matches!(lifecycle.phase, StagePhase::Producing) {
                continue;
            }
            return Some(self.build_stage_runtime_snapshot(&key, lifecycle, metrics));
        }
        None
    }

    pub async fn active_hls_preview_stage_keys(&self) -> HashSet<StageKey> {
        let preview_ids = self.hls_preview_pipeline_ids().await;
        let active_preview_ids = {
            let consumers = self.hls.consumers.read().await;
            preview_ids
                .into_iter()
                .filter(|preview_id| {
                    consumers
                        .get(&hls_preview_registry_key(preview_id))
                        .is_some_and(|consumer| !consumer.cancel_token.is_cancelled())
                })
                .collect::<Vec<_>>()
        };
        let active_codecs = self
            .ingests
            .active
            .read()
            .await
            .iter()
            .filter_map(|(pipeline_id, ingest)| {
                ingest
                    .metadata()
                    .video
                    .map(|video| (pipeline_id.clone(), video.codec))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let input_codecs = self
            .ingests
            .sessions
            .read()
            .await
            .iter()
            .filter_map(|(input_id, ingest)| {
                ingest
                    .metadata()
                    .video
                    .map(|video| (input_id.clone(), video.codec))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut needed = HashSet::new();

        for pipeline_id in active_preview_ids {
            let ingest_codec =
                if let Some(input_id) = input_id_from_hls_preview_resource_id(&pipeline_id) {
                    input_codecs.get(input_id).map(String::as_str)
                } else {
                    active_codecs.get(&pipeline_id).map(String::as_str)
                };
            let backend_policy = self.backend_policy();
            let Some(plan) = crate::planner::graph_plan::plan_hls_preview_graph(
                &pipeline_id,
                ingest_codec,
                &backend_policy,
            ) else {
                continue;
            };

            for stage in plan.stages {
                if stage.kind != StageKind::Source {
                    needed.insert(stage.key);
                }
            }
        }

        needed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::engine::MediaEngine;

    fn test_key() -> StageKey {
        StageKey::new("pipe-a", StageKind::source())
    }

    #[tokio::test]
    async fn register_input_queue_is_noop_when_runtime_missing() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .register_input_queue(key.clone(), Arc::new(MemoryQueue::new()))
            .await;
        assert!(engine.stages.runtimes.read().await.get(&key).is_none());
    }

    #[tokio::test]
    async fn register_and_remove_input_queue_round_trips_on_existing_runtime() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Registered,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;

        engine
            .register_input_queue(key.clone(), Arc::new(MemoryQueue::new()))
            .await;
        assert!(
            engine.stages.runtimes.read().await[&key]
                .input_queue
                .is_some()
        );

        engine.remove_input_queue(&key).await;
        assert!(
            engine.stages.runtimes.read().await[&key]
                .input_queue
                .is_none()
        );
    }

    #[tokio::test]
    async fn remove_input_queue_is_noop_when_runtime_missing() {
        let engine = MediaEngine::new();
        engine.remove_input_queue(&test_key()).await;
    }

    #[tokio::test]
    async fn register_and_remove_pipe_metrics_round_trips_on_existing_runtime() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Registered,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;

        engine
            .register_pipe_metrics(key.clone(), Arc::new(PipeMetrics::default()))
            .await;
        assert!(
            engine.stages.runtimes.read().await[&key]
                .pipe_metrics
                .is_some()
        );

        engine.remove_pipe_metrics(&key).await;
        assert!(
            engine.stages.runtimes.read().await[&key]
                .pipe_metrics
                .is_none()
        );
    }

    #[tokio::test]
    async fn remove_pipe_metrics_is_noop_when_runtime_missing() {
        let engine = MediaEngine::new();
        engine.remove_pipe_metrics(&test_key()).await;
    }

    #[tokio::test]
    async fn get_or_create_non_ring_stage_runtime_creates_on_first_call() {
        let engine = MediaEngine::new();
        let key = test_key();
        let (lifecycle, _metrics) = engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Registered,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;
        assert_eq!(lifecycle.current_phase(), StagePhase::Registered);
        assert_eq!(
            lifecycle.current_backend(),
            StageBackendKind::InternalFfmpeg
        );
    }

    #[tokio::test]
    async fn get_or_create_non_ring_stage_runtime_reuses_when_not_cancelled() {
        let engine = MediaEngine::new();
        let key = test_key();
        let (first, _) = engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Registered,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;
        let (second, _) = engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Producing,
                StageBackendKind::ExternalFfmpeg,
                CancellationToken::new(),
            )
            .await;
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.current_phase(), StagePhase::Registered);
    }

    #[tokio::test]
    async fn get_or_create_non_ring_stage_runtime_replaces_when_cancelled() {
        let engine = MediaEngine::new();
        let key = test_key();
        let stale_cancel = CancellationToken::new();
        let (first, _) = engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Registered,
                StageBackendKind::InternalFfmpeg,
                stale_cancel.clone(),
            )
            .await;
        stale_cancel.cancel();

        let (second, _) = engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Producing,
                StageBackendKind::ExternalFfmpeg,
                CancellationToken::new(),
            )
            .await;
        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(second.current_phase(), StagePhase::Producing);
    }

    #[tokio::test]
    async fn remove_stage_metrics_is_idempotent_on_missing_key() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine.remove_stage_metrics(&key).await;
        engine.remove_stage_metrics(&key).await;
    }

    #[tokio::test]
    async fn remove_stage_metrics_removes_entry() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine.get_or_create_stage_metrics(key.clone()).await;
        assert!(engine.stages.metrics.read().await.contains_key(&key));
        engine.remove_stage_metrics(&key).await;
        assert!(!engine.stages.metrics.read().await.contains_key(&key));
    }

    #[tokio::test]
    async fn remove_stage_runtime_is_idempotent_on_missing_key() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine.remove_stage_runtime(&key).await;
        engine.remove_stage_runtime(&key).await;
    }

    #[tokio::test]
    async fn remove_stage_runtime_removes_entry() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Registered,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;
        assert!(engine.stages.runtimes.read().await.contains_key(&key));
        engine.remove_stage_runtime(&key).await;
        assert!(!engine.stages.runtimes.read().await.contains_key(&key));
    }

    #[tokio::test]
    async fn get_or_create_stage_lifecycle_with_backend_is_idempotent() {
        let engine = MediaEngine::new();
        let key = test_key();
        let first = engine
            .get_or_create_stage_lifecycle_with_backend(
                key.clone(),
                StagePhase::Registered,
                StageBackendKind::HlsSegmenter,
            )
            .await;
        let second = engine
            .get_or_create_stage_lifecycle_with_backend(
                key.clone(),
                StagePhase::Producing,
                StageBackendKind::Recording,
            )
            .await;
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(second.current_backend(), StageBackendKind::HlsSegmenter);
    }

    #[tokio::test]
    async fn get_or_create_stage_lifecycle_with_backend_prefers_existing_runtime_lifecycle() {
        let engine = MediaEngine::new();
        let key = test_key();
        let (runtime_lifecycle, _) = engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Registered,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;

        let lifecycle = engine
            .get_or_create_stage_lifecycle_with_backend(
                key.clone(),
                StagePhase::Producing,
                StageBackendKind::Recording,
            )
            .await;

        assert!(Arc::ptr_eq(&runtime_lifecycle, &lifecycle));
        assert!(!engine.stages.lifecycles.read().await.contains_key(&key));
    }

    #[tokio::test]
    async fn remove_stage_lifecycle_is_idempotent_on_missing_key() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine.remove_stage_lifecycle(&key).await;
        engine.remove_stage_lifecycle(&key).await;
    }

    #[tokio::test]
    async fn remove_stage_lifecycle_removes_entry() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
            .await;
        assert!(engine.stages.lifecycles.read().await.contains_key(&key));
        engine.remove_stage_lifecycle(&key).await;
        assert!(!engine.stages.lifecycles.read().await.contains_key(&key));
    }

    #[tokio::test]
    async fn stage_lifecycle_snapshot_is_none_when_key_is_absent_from_both_maps() {
        let engine = MediaEngine::new();
        assert!(engine.stage_lifecycle_snapshot(&test_key()).await.is_none());
    }

    #[tokio::test]
    async fn stage_lifecycle_snapshot_falls_back_to_lifecycles_map_when_no_runtime() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_stage_lifecycle(key.clone(), StagePhase::Producing)
            .await;

        let snapshot = engine
            .stage_lifecycle_snapshot(&key)
            .await
            .expect("lifecycle-only entry should be found via fallback");
        assert_eq!(snapshot.phase, StagePhase::Producing);
    }

    #[tokio::test]
    async fn stage_lifecycle_snapshot_prefers_runtime_map_when_both_present() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_stage_lifecycle(key.clone(), StagePhase::Failed)
            .await;
        engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Producing,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;

        let snapshot = engine.stage_lifecycle_snapshot(&key).await.unwrap();
        assert_eq!(snapshot.phase, StagePhase::Producing);
    }

    #[tokio::test]
    async fn stage_runtime_snapshot_has_no_capacity_fields_outside_capacity_phases() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Producing,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;

        let snapshot = engine.stage_runtime_snapshot(&key).await.unwrap();
        assert!(snapshot.capacity_permits_total.is_none());
        assert!(snapshot.capacity_permits_available.is_none());
        assert!(snapshot.capacity_wait_ms.is_none());
    }

    #[tokio::test]
    async fn stage_runtime_snapshot_has_capacity_fields_during_waiting_for_capacity() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::WaitingForCapacity {
                    backend: StageBackendKind::ExternalFfmpeg,
                },
                StageBackendKind::ExternalFfmpeg,
                CancellationToken::new(),
            )
            .await;

        let snapshot = engine.stage_runtime_snapshot(&key).await.unwrap();
        assert!(snapshot.capacity_permits_total.is_some());
        assert!(snapshot.capacity_permits_available.is_some());
        assert!(snapshot.capacity_wait_ms.is_some());
    }

    #[tokio::test]
    async fn egress_blocked_by_stage_snapshot_is_none_for_missing_key() {
        let engine = MediaEngine::new();
        assert!(
            engine
                .egress_blocked_by_stage_snapshot(&test_key())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn egress_blocked_by_stage_snapshot_is_none_when_producing() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::Producing,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;
        assert!(
            engine
                .egress_blocked_by_stage_snapshot(&key)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn egress_blocked_by_stage_snapshot_is_some_when_not_yet_producing() {
        let engine = MediaEngine::new();
        let key = test_key();
        engine
            .get_or_create_non_ring_stage_runtime(
                key.clone(),
                StagePhase::FirstInput,
                StageBackendKind::InternalFfmpeg,
                CancellationToken::new(),
            )
            .await;
        let snapshot = engine
            .egress_blocked_by_stage_snapshot(&key)
            .await
            .expect("non-producing stage should report as blocking");
        assert_eq!(snapshot.phase, StagePhase::FirstInput);
    }
}
