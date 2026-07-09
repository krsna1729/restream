use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::domain::stage::StageKey;
use crate::media::engine::{ActiveEgress, MediaEngine};
use crate::media::stage_lifecycle::{
    StageBackendKind, StageLifecycle, StageLifecycleSnapshot, StagePhase,
};
use crate::media::stage_metrics::StageMetrics;
use crate::runtime::stage::StageRuntimeSnapshot;

impl MediaEngine {
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
            backend: lifecycle.backend.clone(),
            phase: lifecycle.phase.clone(),
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
}
