//! API/runtime telemetry adapters for engine-, pipeline-, and stage-scoped
//! metrics views.
//! This file owns JSON shaping for queue, ring, and stage telemetry while the
//! runtime layer continues to own the underlying counters and registries.

use super::telemetry_projection as api_view_models;
use crate::domain::stage::StageKey;
use crate::media::engine::MediaEngine;
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::stage_metrics::StageMetrics;
use std::collections::HashSet;
use std::sync::Arc;

async fn stage_telemetry_keys(engine: &MediaEngine) -> Vec<StageKey> {
    let mut keys = HashSet::new();
    {
        let runtimes = engine.stages.runtimes.read().await;
        keys.extend(runtimes.keys().cloned());
    }
    {
        let metrics = engine.stages.metrics.read().await;
        keys.extend(metrics.keys().cloned());
    }
    let mut keys: Vec<_> = keys.into_iter().collect();
    keys.sort_by_key(|key| key.to_string());
    keys
}

async fn stage_metrics_for(engine: &MediaEngine, key: &StageKey) -> Option<Arc<StageMetrics>> {
    if let Some(runtime) = engine.stages.runtimes.read().await.get(key).cloned() {
        return Some(runtime.metrics);
    }
    engine.stages.metrics.read().await.get(key).cloned()
}

async fn stage_pipe_metrics_for(engine: &MediaEngine, key: &StageKey) -> Option<Arc<PipeMetrics>> {
    engine
        .stages
        .runtimes
        .read()
        .await
        .get(key)
        .and_then(|runtime| runtime.pipe_metrics.clone())
}

pub(crate) async fn engine_telemetry(engine: &MediaEngine) -> serde_json::Value {
    let generated_at = chrono::Utc::now().to_rfc3339();
    let ingests = engine.ingests.active.read().await;
    let egresses = engine.egresses.active.read().await;
    let runtimes = engine.stages.runtimes.read().await;
    let pipelines = engine.ingests.pipelines.read().await;
    let ts_muxers = engine.stages.ts_muxers.read().await;
    let egress_queues = engine.egresses.queues.read().await;
    let stage_keys = stage_telemetry_keys(engine).await;

    // Fetch lifecycle snapshots for all registered stages.
    let mut lifecycle_snapshots = std::collections::HashMap::new();
    for key in &stage_keys {
        if let Some(snap) = engine.stage_runtime_snapshot(key).await {
            lifecycle_snapshots.insert(key.clone(), snap);
        }
    }

    let ingest_arr: Vec<serde_json::Value> = ingests
        .iter()
        .map(|(pid, ingest)| api_view_models::ingest_telemetry_json(pid, ingest))
        .collect();

    let mut stage_arr = Vec::with_capacity(stage_keys.len());
    for key in &stage_keys {
        let Some(metrics) = stage_metrics_for(engine, key).await else {
            continue;
        };
        let row = api_view_models::stage_telemetry_row_json(
            key,
            serde_json::to_value(metrics.snapshot()).unwrap_or_default(),
            runtimes
                .get(key)
                .and_then(|runtime| runtime.pipe_metrics.as_ref())
                .map(|pm| serde_json::to_value(pm.snapshot()).unwrap_or_default()),
            None,
            None,
            lifecycle_snapshots.get(key),
        );
        stage_arr.push(row);
    }

    let egress_arr: Vec<serde_json::Value> = egresses
        .values()
        .map(|egress| {
            api_view_models::egress_runtime_json(
                egress,
                true,
                ingests.contains_key(egress.pipeline_id.as_str()),
                None,
            )
        })
        .collect();

    let source_rings: Vec<serde_json::Value> = pipelines
        .iter()
        .map(|(pipeline_id, ring)| api_view_models::source_ring_telemetry_json(pipeline_id, ring))
        .collect();
    let transcoder_rings: Vec<serde_json::Value> = runtimes
        .iter()
        .filter_map(|(key, runtime)| {
            runtime.ring.as_ref().map(|ring| {
                api_view_models::transcoder_ring_telemetry_json(
                    key,
                    ring,
                    !runtime.cancel.is_cancelled(),
                )
            })
        })
        .collect();
    let ts_muxer_rings: Vec<serde_json::Value> = ts_muxers
        .iter()
        .map(|(stage_key, stage)| {
            api_view_models::ts_muxer_ring_telemetry_json(
                stage_key,
                &stage.ring,
                !stage.cancel.is_cancelled(),
            )
        })
        .collect();
    let retained_payload_bytes = source_rings
        .iter()
        .chain(transcoder_rings.iter())
        .chain(ts_muxer_rings.iter())
        .filter_map(|entry| entry["payloadStats"]["payloadBytes"].as_u64())
        .sum::<u64>();

    let avio_input_queues: Vec<serde_json::Value> = runtimes
        .iter()
        .filter_map(|(key, runtime)| runtime.input_queue.as_ref().map(|queue| (key, queue)))
        .map(|(key, queue)| {
            let stats = queue.stats();
            api_view_models::avio_input_queue_json(
                key,
                stats.len,
                stats.capacity,
                stats.high_water_bytes,
                stats.blocked_writes,
                stats.blocked_write_us,
            )
        })
        .collect();
    let avio_egress_queues: Vec<serde_json::Value> = egress_queues
        .iter()
        .map(|(output_id, queue)| {
            let stats = queue.stats();
            api_view_models::avio_egress_queue_json(
                output_id,
                stats.len,
                stats.capacity,
                stats.high_water_bytes,
                stats.blocked_writes,
                stats.blocked_write_us,
            )
        })
        .collect();
    let avio_total_len_bytes: usize = avio_input_queues
        .iter()
        .chain(avio_egress_queues.iter())
        .filter_map(|entry| entry["lenBytes"].as_u64())
        .map(|value| value as usize)
        .sum();
    let avio_total_capacity_bytes: usize = avio_input_queues
        .iter()
        .chain(avio_egress_queues.iter())
        .filter_map(|entry| entry["capacityBytes"].as_u64())
        .map(|value| value as usize)
        .sum();

    api_view_models::engine_telemetry_json(
        generated_at,
        ingest_arr,
        stage_arr,
        egress_arr,
        runtimes.len(),
        api_view_models::memory_accounting_json(
            retained_payload_bytes,
            source_rings,
            transcoder_rings,
            ts_muxer_rings,
            avio_total_len_bytes,
            avio_total_capacity_bytes,
            avio_input_queues,
            avio_egress_queues,
        ),
    )
}

pub(crate) async fn pipeline_telemetry(
    engine: &MediaEngine,
    pipeline_id: &str,
) -> serde_json::Value {
    let generated_at = chrono::Utc::now().to_rfc3339();
    let ingests = engine.ingests.active.read().await;
    let egresses = engine.egresses.active.read().await;
    let pipelines = engine.ingests.pipelines.read().await;
    let runtimes = engine.stages.runtimes.read().await;
    let stage_keys: Vec<_> = stage_telemetry_keys(engine)
        .await
        .into_iter()
        .filter(|key| key.pipeline.as_str() == pipeline_id)
        .collect();

    // Pre-fetch lifecycle snapshots for stages belonging to this pipeline.
    let mut lifecycle_snapshots = std::collections::HashMap::new();
    for key in &stage_keys {
        if let Some(snap) = engine.stage_runtime_snapshot(key).await {
            lifecycle_snapshots.insert(key.clone(), snap);
        }
    }

    let ingest = ingests
        .get(pipeline_id)
        .map(|ingest| api_view_models::pipeline_ingest_telemetry_json(ingest.as_ref()));

    let ring_info = pipelines
        .get(pipeline_id)
        .map(|ring| api_view_models::pipeline_source_ring_json(ring));

    let mut stages = Vec::with_capacity(stage_keys.len());
    for key in &stage_keys {
        let Some(metrics) = stage_metrics_for(engine, key).await else {
            continue;
        };
        let mut val = api_view_models::stage_telemetry_row_json(
            key,
            serde_json::to_value(metrics.snapshot()).unwrap_or_default(),
            runtimes
                .get(key)
                .and_then(|runtime| runtime.pipe_metrics.as_ref())
                .map(|pm| serde_json::to_value(pm.snapshot()).unwrap_or_default()),
            None,
            None,
            lifecycle_snapshots.get(key),
        );
        if let Some(runtime) = runtimes.get(key) {
            val["active"] = serde_json::json!(!runtime.cancel.is_cancelled());
            if let Some(ring) = runtime.ring.as_ref() {
                val["payloadStats"] = api_view_models::ring_payload_stats_json(ring);
            }
        }
        val.as_object_mut()
            .expect("stage telemetry rows are objects")
            .remove("stageKey");
        val.as_object_mut()
            .expect("stage telemetry rows are objects")
            .remove("pipelineId");
        stages.push(val);
    }

    let pipeline_egresses: Vec<serde_json::Value> = egresses
        .values()
        .filter(|egress| egress.pipeline_id == pipeline_id)
        .map(|egress| {
            api_view_models::egress_runtime_json(
                egress,
                true,
                ingests.contains_key(pipeline_id),
                None,
            )
        })
        .collect();

    api_view_models::pipeline_telemetry_json(
        generated_at,
        pipeline_id,
        ingest,
        ring_info,
        stages,
        pipeline_egresses,
    )
}

pub(crate) async fn stage_telemetry_by_display(
    engine: &MediaEngine,
    display: &str,
) -> Option<serde_json::Value> {
    let key = stage_telemetry_keys(engine)
        .await
        .into_iter()
        .find(|key| key.to_string() == display)?;
    let metrics = stage_metrics_for(engine, &key).await?;
    let pipe = stage_pipe_metrics_for(engine, &key)
        .await
        .map(|pm| serde_json::to_value(pm.snapshot()).unwrap_or_default());

    // Fetch lifecycle snapshot for detailed phase/capacity info.
    let lifecycle = engine.stage_runtime_snapshot(&key).await;

    Some(api_view_models::single_stage_telemetry_json(
        chrono::Utc::now().to_rfc3339(),
        &key,
        serde_json::to_value(metrics.snapshot()).unwrap_or_default(),
        pipe,
        lifecycle.as_ref(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::avio::MemoryQueue;
    use crate::media::pipe_metrics::PipeMetrics;
    use crate::media::ring_buffer::RingBuffer;
    use crate::media::stage_lifecycle::StagePhase;
    use crate::media::stage_runtime::StageRuntimeManager;
    use std::sync::Arc;

    #[tokio::test]
    async fn stage_telemetry_reads_pipe_metrics_from_stage_runtime() {
        let engine = Arc::new(MediaEngine::new());
        let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
        let manager = StageRuntimeManager::new(engine.clone());
        manager
            .ensure_stage(key.clone(), Arc::new(RingBuffer::new(4)), None)
            .await;
        let pipe_metrics = Arc::new(PipeMetrics::default());
        pipe_metrics.record_stall(1_500);
        engine
            .register_pipe_metrics(key.clone(), pipe_metrics.clone())
            .await;

        let telemetry = stage_telemetry_by_display(&engine, &key.to_string())
            .await
            .expect("stage telemetry should exist");

        assert_eq!(telemetry["pipeMetrics"]["stalls"], 1);
        assert_eq!(telemetry["pipeMetrics"]["stallUs"], 1_500);
    }

    #[tokio::test]
    async fn engine_telemetry_reads_input_queues_from_stage_runtime() {
        let engine = Arc::new(MediaEngine::new());
        let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
        let manager = StageRuntimeManager::new(engine.clone());
        manager
            .ensure_stage(key.clone(), Arc::new(RingBuffer::new(4)), None)
            .await;
        let input_queue = Arc::new(MemoryQueue::new_with_capacity(128));
        input_queue.write_sync(b"queued bytes");
        engine.register_input_queue(key.clone(), input_queue).await;

        let telemetry = engine_telemetry(&engine).await;
        let queues = telemetry["memoryAccounting"]["avioQueues"]["inputQueues"]
            .as_array()
            .expect("input queues should be serialized");

        let queue = queues
            .iter()
            .find(|queue| queue["stageKey"] == key.to_string())
            .expect("runtime input queue should be present");
        assert_eq!(queue["lenBytes"], b"queued bytes".len() as u64);
        assert_eq!(queue["capacityBytes"], 128);
    }

    #[tokio::test]
    async fn telemetry_reads_runtime_stage_after_metrics_side_map_removed() {
        let engine = Arc::new(MediaEngine::new());
        let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
        let manager = StageRuntimeManager::new(engine.clone());
        let (handle, _) = manager
            .ensure_stage(key.clone(), Arc::new(RingBuffer::new(4)), None)
            .await;
        handle.metrics.record_in(64);
        handle.metrics.record_out(32);
        handle.lifecycle.transition(StagePhase::RunningNoOutputYet);
        engine.stages.metrics.write().await.remove(&key);
        engine.stages.lifecycles.write().await.remove(&key);

        let engine_view = engine_telemetry(&engine).await;
        let stage = engine_view["stages"]
            .as_array()
            .expect("engine stages should be serialized")
            .iter()
            .find(|stage| stage["stageKey"] == key.to_string())
            .expect("runtime-backed stage should appear in engine telemetry");
        assert_eq!(stage["metrics"]["bytesIn"], 64);
        assert_eq!(stage["metrics"]["bytesOut"], 32);
        assert_eq!(stage["lifecycle"]["phase"], "runningNoOutputYet");

        let pipeline_view = pipeline_telemetry(&engine, "pipe-1").await;
        let stage = pipeline_view["stages"]
            .as_array()
            .expect("pipeline stages should be serialized")
            .iter()
            .find(|stage| stage["kind"] == key.kind.to_string())
            .expect("runtime-backed stage should appear in pipeline telemetry");
        assert_eq!(stage["metrics"]["bytesIn"], 64);
        assert_eq!(stage["lifecycle"]["phase"], "runningNoOutputYet");

        let single = stage_telemetry_by_display(&engine, &key.to_string())
            .await
            .expect("runtime-backed stage should resolve by display key");
        assert_eq!(single["metrics"]["bytesOut"], 32);
        assert_eq!(single["lifecycle"]["phase"], "runningNoOutputYet");
    }
}
