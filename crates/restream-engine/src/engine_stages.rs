//! `MediaEngine` stage operations that need more than the stage registry
//! (the shared TS muxer start, capacity permits, HLS preview consumers,
//! ingest codecs), and forwards to the stage registry's operations
//! (`stage_registry.rs`) for the application and the engine's callers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::engine::MediaEngine;
use crate::engine_registries::ActiveEgress;
use crate::preview_ids::{
    hls_preview_registry_key, input_id_from_hls_preview_resource_id, preview_pipeline,
};
use crate::stage_lifecycle::{
    StageBackendKind, StageLifecycle, StageLifecycleSnapshot, StagePhase,
};
use crate::stage_metrics::StageMetrics;
use crate::stage_registry::{StageLease, StageRuntime, TsMuxerStage};
use crate::ts_chunk_ring::TsChunkRing;
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_domain::runtime::stage::StageRuntimeSnapshot;
use restream_sync::RingBuffer;
use tokio_util::sync::CancellationToken;

impl MediaEngine {
    /// Runs `f` over every stage runtime; `f` runs under the runtime map's
    /// read guard and must not wait.
    pub async fn with_stage_runtimes<R>(
        &self,
        f: impl FnOnce(&HashMap<StageKey, StageRuntime>) -> R,
    ) -> R {
        self.stages.with_runtimes(f).await
    }

    /// Runs `f` over the side-map stage metrics (stages without a runtime);
    /// `f` runs under the metrics map's read guard and must not wait.
    pub async fn with_stage_metrics<R>(
        &self,
        f: impl FnOnce(&HashMap<StageKey, Arc<StageMetrics>>) -> R,
    ) -> R {
        self.stages.with_metrics(f).await
    }

    /// Runs `f` over every shared TS muxer, keyed by `<pipeline>:<stage>`;
    /// `f` runs under the muxer map's read guard and must not wait.
    pub async fn with_ts_muxer_stages<R>(
        &self,
        f: impl FnOnce(&HashMap<String, TsMuxerStage>) -> R,
    ) -> R {
        self.stages.with_ts_muxers(f).await
    }

    pub async fn get_or_create_non_ring_stage_runtime(
        &self,
        key: StageKey,
        initial: StagePhase,
        backend: StageBackendKind,
        cancel: CancellationToken,
    ) -> (Arc<StageLifecycle>, Arc<StageMetrics>) {
        self.stages
            .get_or_create_non_ring_stage_runtime(key, initial, backend, cancel)
            .await
    }

    pub async fn remove_stage_runtime(&self, key: &StageKey) {
        self.stages.remove_stage_runtime(key).await;
    }

    /// The shared TS muxer for `stage_key`, and the caller's lease on it: the
    /// muxer stops when its last lease drops.
    pub async fn get_or_create_ts_muxer_stage(
        &self,
        pipeline_id: &str,
        stage_key: &str,
        source_ring: Arc<RingBuffer>,
    ) -> (Arc<TsChunkRing>, StageLease) {
        let key = format!("{}:{}", pipeline_id, stage_key);
        self.stages
            .get_or_create_ts_muxer(key, |cancel| {
                crate::shared_ts_muxer::start_shared_ts_muxer(
                    pipeline_id,
                    stage_key,
                    source_ring,
                    self.ingests.clone(),
                    self.config.ts_ring_capacity,
                    cancel,
                )
            })
            .await
    }

    /// `StageRegistry::assign_srt_egress_muxer_stage` with the configured
    /// shard limits.
    pub async fn assign_srt_egress_muxer_stage(
        &self,
        pipeline_id: &str,
        encoding: &str,
        output_id: &str,
        attempt_id: u64,
    ) -> String {
        self.stages
            .assign_srt_egress_muxer_stage(
                pipeline_id,
                encoding,
                output_id,
                attempt_id,
                self.config.srt_egress_muxer_max_outputs_per_shard,
                self.config.srt_egress_muxer_max_shards,
            )
            .await
    }

    /// `StageRegistry::release_srt_egress_muxer_stage` with the configured
    /// shard limit.
    pub async fn release_srt_egress_muxer_stage(
        &self,
        pipeline_id: &str,
        encoding: &str,
        output_id: &str,
        attempt_id: u64,
    ) {
        self.stages
            .release_srt_egress_muxer_stage(
                pipeline_id,
                encoding,
                output_id,
                attempt_id,
                self.config.srt_egress_muxer_max_outputs_per_shard,
            )
            .await;
    }

    fn build_stage_runtime_snapshot(
        &self,
        key: &StageKey,
        lifecycle: &StageLifecycleSnapshot,
        metrics: &StageMetrics,
    ) -> StageRuntimeSnapshot {
        crate::stage_registry::stage_runtime_snapshot(
            key,
            lifecycle,
            metrics,
            &self.runtime.external_ffmpeg_semaphore,
            self.config.external_ffmpeg_permits,
        )
    }

    pub async fn stage_runtime_snapshot(&self, key: &StageKey) -> Option<StageRuntimeSnapshot> {
        self.stages
            .stage_runtime_snapshot(
                key,
                &self.runtime.external_ffmpeg_semaphore,
                self.config.external_ffmpeg_permits,
            )
            .await
    }

    /// Return runtime snapshots for all stages belonging to the given pipeline.
    pub async fn pipeline_stage_runtime_snapshots(
        &self,
        pipeline_id: &str,
    ) -> Vec<StageRuntimeSnapshot> {
        let keys = self.stages.pipeline_stage_keys(pipeline_id).await;

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
        let (lifecycle, metrics) = self.stages.stage_snapshot_parts(key).await?;
        if matches!(lifecycle.phase, StagePhase::Producing) {
            return None;
        }
        Some(self.build_stage_runtime_snapshot(key, &lifecycle, &metrics))
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
            let Some((lifecycle, metrics)) = self.stages.stage_snapshot_parts(&key).await else {
                continue;
            };
            if matches!(lifecycle.phase, StagePhase::Producing) {
                continue;
            }
            return Some(self.build_stage_runtime_snapshot(&key, &lifecycle, &metrics));
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
            // A passthrough preview (`<id>.hevc`) plans no stage.
            if preview_pipeline(&pipeline_id).1 {
                continue;
            }
            let ingest_codec =
                if let Some(input_id) = input_id_from_hls_preview_resource_id(&pipeline_id) {
                    input_codecs.get(input_id).map(String::as_str)
                } else {
                    active_codecs.get(&pipeline_id).map(String::as_str)
                };
            let Some(plan) =
                restream_domain::planner::plan_hls_preview_graph(&pipeline_id, ingest_codec)
            else {
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
mod tests;
