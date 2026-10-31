//! API/runtime telemetry adapters for engine-, pipeline-, and stage-scoped
//! metrics views.
//! This file owns JSON shaping for queue, ring, and stage telemetry while the
//! runtime layer continues to own the underlying counters and registries.

use super::telemetry_projection as api_view_models;
use restream_domain::domain::stage::StageKey;
use restream_engine::engine::MediaEngine;
use restream_engine::pipe_metrics::PipeMetrics;
use restream_engine::stage_metrics::StageMetrics;
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
    // Read and released before the other maps: no new lock nesting.
    let input_arr: Vec<serde_json::Value> = engine
        .ingests
        .sessions
        .read()
        .await
        .values()
        .map(|session| api_view_models::input_telemetry_json(session))
        .collect();
    let ingests = engine.ingests.active.read().await;
    let egresses = engine.egresses.active.read().await;
    let runtimes = engine.stages.runtimes.read().await;
    let pipelines = engine.ingests.pipelines.read().await;
    let ts_muxers = engine.stages.ts_muxers.read().await;
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
                &stage.muxer.ring,
                !stage.muxer.cancel.is_cancelled(),
            )
        })
        .collect();
    let retained_payload_bytes = source_rings
        .iter()
        .chain(transcoder_rings.iter())
        .chain(ts_muxer_rings.iter())
        .filter_map(|entry| entry["payloadStats"]["payloadBytes"].as_u64())
        .sum::<u64>();

    api_view_models::engine_telemetry_json(
        generated_at,
        ingest_arr,
        input_arr,
        stage_arr,
        egress_arr,
        runtimes.len(),
        api_view_models::memory_accounting_json(
            retained_payload_bytes,
            source_rings,
            transcoder_rings,
            ts_muxer_rings,
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

    let delivery = feed_delivery(
        egresses
            .values()
            .filter(|egress| egress.pipeline_id == pipeline_id)
            .map(delivery_input),
    );
    let mut value = api_view_models::pipeline_telemetry_json(
        generated_at,
        pipeline_id,
        ingest,
        ring_info,
        stages,
        pipeline_egresses,
    );
    value["delivery"] = super::common::to_json(&delivery);
    value
}

/// One output's delivery sample for feed grouping.
#[derive(Debug, Clone)]
pub(super) struct DeliveryInput {
    pub(super) feed: String,
    pub(super) protocol: String,
    /// `(delivered_bps, delivery_ratio)` once the output has a sample.
    pub(super) rate: Option<(f64, f64)>,
}

pub(super) fn delivery_input(egress: &restream_engine::engine::ActiveEgress) -> DeliveryInput {
    let quality = restream_sync::lock::lock(&egress.quality);
    DeliveryInput {
        feed: egress
            .terminal_stage_key
            .as_ref()
            .map_or_else(|| "unknown".to_string(), ToString::to_string),
        protocol: egress.protocol.clone(),
        rate: quality.delivered_bps.zip(quality.delivery_ratio),
    }
}

/// Delivery of one feed: outputs that read the same terminal stage over the
/// same protocol.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FeedDelivery {
    pub(super) feed: String,
    pub(super) protocol: String,
    pub(super) destinations: usize,
    pub(super) rated: usize,
    pub(super) delivered: usize,
    pub(super) floor: f64,
    pub(super) ratio_min: Option<f64>,
    /// Jain's fairness over the rated outputs' rates; none until one is rated.
    pub(super) jain: Option<f64>,
}

/// Per-feed delivery fairness: outputs reading the same terminal stage over
/// the same protocol share one feed and one wire format, so their
/// peer-acknowledged rates are comparable with Jain's index and counted
/// against the delivery floor. (RTMP and SRT outputs of one stage carry
/// different framing overhead, so they form separate rows.) Built from each output's latest
/// quality sample; outputs without a sample yet are listed but not rated.
pub(super) fn feed_delivery(inputs: impl IntoIterator<Item = DeliveryInput>) -> Vec<FeedDelivery> {
    const DELIVERY_FLOOR: f64 = 0.95;
    // (feed, protocol) -> each output's optional (delivered_bps, ratio).
    type Feeds = std::collections::BTreeMap<(String, String), Vec<Option<(f64, f64)>>>;
    let mut feeds = Feeds::new();
    for input in inputs {
        feeds
            .entry((input.feed, input.protocol))
            .or_default()
            .push(input.rate);
    }
    feeds
        .into_iter()
        .map(|((feed, protocol), outputs)| {
            let rated: Vec<(f64, f64)> = outputs.iter().flatten().copied().collect();
            let rates: Vec<f64> = rated.iter().map(|(rate, _)| *rate).collect();
            FeedDelivery {
                feed,
                protocol,
                destinations: outputs.len(),
                rated: rated.len(),
                delivered: rated
                    .iter()
                    .filter(|(_, ratio)| *ratio >= DELIVERY_FLOOR)
                    .count(),
                floor: DELIVERY_FLOOR,
                ratio_min: rated.iter().map(|(_, ratio)| *ratio).reduce(f64::min),
                jain: restream_engine::egress::delivery::jain_index(&rates),
            }
        })
        .collect()
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

    #[test]
    fn feed_delivery_groups_by_terminal_stage_and_rates_fairness() {
        let output = |feed: &str, protocol: &str, rate: Option<(f64, f64)>| DeliveryInput {
            feed: feed.to_string(),
            protocol: protocol.to_string(),
            rate,
        };
        let rows = feed_delivery([
            output("p:source", "rtmp", Some((8e6, 1.01))),
            output("p:source", "rtmp", Some((8e6, 1.0))),
            output("p:720p", "rtmp", Some((4e6, 1.0))),
            output("p:720p", "rtmp", Some((1e6, 0.25))),
            output("p:720p", "rtmp", None),
        ]);
        assert_eq!(rows.len(), 2);
        let p720 = &rows[0];
        assert_eq!(p720.feed, "p:720p");
        assert_eq!(p720.destinations, 3);
        assert_eq!(p720.rated, 2, "an output without a sample is not rated");
        assert_eq!(p720.delivered, 1);
        assert_eq!(p720.ratio_min, Some(0.25));
        assert!(p720.jain.unwrap() < 0.8);
        let source = &rows[1];
        assert_eq!(source.delivered, 2);
        assert_eq!(source.jain, Some(1.0));

        // Same stage, different protocol: separate rows, so different wire
        // overhead does not read as unfairness.
        let rows = feed_delivery([
            output("p:source", "rtmp", Some((8.1e6, 1.01))),
            output("p:source", "srt", Some((8.6e6, 1.0))),
        ]);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.jain == Some(1.0)));
    }
    use restream_domain::domain::stage::{StageKey, StageKind};
    use restream_engine::pipe_metrics::PipeMetrics;
    use restream_engine::stage_lifecycle::StagePhase;
    use restream_engine::stage_runtime::StageRuntimeManager;
    use restream_sync::RingBuffer;
    use std::sync::Arc;

    #[tokio::test]
    async fn stage_telemetry_reads_pipe_metrics_from_stage_runtime() {
        let engine = Arc::new(MediaEngine::new());
        let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
        let manager = StageRuntimeManager::new(engine.clone());
        let _stage = manager
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
    async fn telemetry_reads_runtime_stage_after_metrics_side_map_removed() {
        let engine = Arc::new(MediaEngine::new());
        let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
        let manager = StageRuntimeManager::new(engine.clone());
        let (handle, _lease2, _) = manager
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
