//! API/runtime telemetry adapters for engine-, pipeline-, and stage-scoped
//! metrics views.
//! This file owns JSON shaping for queue, ring, and stage telemetry while the
//! runtime layer continues to own the underlying counters and registries.

use super::telemetry_projection as api_view_models;
use restream_domain::domain::stage::StageKey;
use restream_engine::engine::MediaEngine;
use restream_engine::engine_registries::StageRuntime;
use restream_engine::stage_metrics::StageMetrics;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Owned copies of the stage maps the telemetry views read.
///
/// Every view here reads each engine registry into owned data and releases
/// it before taking the next, never nested and never re-acquired: tokio's
/// fair `RwLock` queues a reader behind any waiting writer, so a view still
/// holding `runtimes` while it asks for `runtimes` again (or for another map
/// a writer holds while waiting on `runtimes`) deadlocks with that writer.
/// Ingest registration takes the engine-wide `ingests.active` under its
/// pipeline's selection, so such a stall would stop every ingest lifecycle.
struct StageMaps {
    runtimes: HashMap<StageKey, StageRuntime>,
    metrics: HashMap<StageKey, Arc<StageMetrics>>,
}

impl StageMaps {
    /// The stages of `pipeline_id`, or every stage when `None`.
    async fn read(engine: &MediaEngine, pipeline_id: Option<&str>) -> Self {
        let wanted = |key: &StageKey| pipeline_id.is_none_or(|id| key.pipeline.as_str() == id);
        let runtimes = engine
            .stages
            .runtimes
            .read()
            .await
            .iter()
            .filter(|(key, _)| wanted(key))
            .map(|(key, runtime)| (key.clone(), runtime.clone()))
            .collect();
        let metrics = engine
            .stages
            .metrics
            .read()
            .await
            .iter()
            .filter(|(key, _)| wanted(key))
            .map(|(key, metrics)| (key.clone(), metrics.clone()))
            .collect();
        Self { runtimes, metrics }
    }

    fn keys(&self) -> Vec<StageKey> {
        let keys: HashSet<&StageKey> = self.runtimes.keys().chain(self.metrics.keys()).collect();
        let mut keys: Vec<StageKey> = keys.into_iter().cloned().collect();
        keys.sort_by_key(|key| key.to_string());
        keys
    }

    /// A runtime's own metrics win over the side map's.
    fn metrics_for(&self, key: &StageKey) -> Option<Arc<StageMetrics>> {
        self.runtimes
            .get(key)
            .map(|runtime| runtime.metrics.clone())
            .or_else(|| self.metrics.get(key).cloned())
    }

    fn pipe_metrics_json(&self, key: &StageKey) -> Option<serde_json::Value> {
        self.runtimes
            .get(key)
            .and_then(|runtime| runtime.pipe_metrics.as_ref())
            .map(|pm| serde_json::to_value(pm.snapshot()).unwrap_or_default())
    }
}

async fn lifecycle_snapshots(
    engine: &MediaEngine,
    keys: &[StageKey],
) -> HashMap<StageKey, restream_domain::runtime::stage::StageRuntimeSnapshot> {
    let mut snapshots = HashMap::with_capacity(keys.len());
    for key in keys {
        if let Some(snapshot) = engine.stage_runtime_snapshot(key).await {
            snapshots.insert(key.clone(), snapshot);
        }
    }
    snapshots
}

pub(crate) async fn engine_telemetry(engine: &MediaEngine) -> serde_json::Value {
    let generated_at = chrono::Utc::now().to_rfc3339();
    // One registry at a time, each guard released before the next await
    // (see `StageMaps`).
    let input_arr: Vec<serde_json::Value> = engine
        .ingests
        .sessions
        .read()
        .await
        .values()
        .map(|session| api_view_models::input_telemetry_json(session))
        .collect();
    let (ingest_arr, ingest_pipelines): (Vec<serde_json::Value>, HashSet<String>) = {
        let ingests = engine.ingests.active.read().await;
        (
            ingests
                .iter()
                .map(|(pid, ingest)| api_view_models::ingest_telemetry_json(pid, ingest))
                .collect(),
            ingests.keys().cloned().collect(),
        )
    };
    let egress_arr: Vec<serde_json::Value> = engine
        .egresses
        .active
        .read()
        .await
        .values()
        .map(|egress| {
            api_view_models::egress_runtime_json(
                egress,
                true,
                ingest_pipelines.contains(egress.pipeline_id.as_str()),
                None,
            )
        })
        .collect();
    let source_rings: Vec<serde_json::Value> = engine
        .ingests
        .pipelines
        .read()
        .await
        .iter()
        .map(|(pipeline_id, ring)| api_view_models::source_ring_telemetry_json(pipeline_id, ring))
        .collect();
    let ts_muxer_rings: Vec<serde_json::Value> = engine
        .stages
        .ts_muxers
        .read()
        .await
        .iter()
        .map(|(stage_key, stage)| {
            api_view_models::ts_muxer_ring_telemetry_json(
                stage_key,
                &stage.muxer.ring,
                !stage.muxer.cancel.is_cancelled(),
            )
        })
        .collect();
    let stages = StageMaps::read(engine, None).await;
    let stage_keys = stages.keys();
    let lifecycle_snapshots = lifecycle_snapshots(engine, &stage_keys).await;

    let stage_arr: Vec<serde_json::Value> = stage_keys
        .iter()
        .filter_map(|key| {
            let metrics = stages.metrics_for(key)?;
            Some(api_view_models::stage_telemetry_row_json(
                key,
                serde_json::to_value(metrics.snapshot()).unwrap_or_default(),
                stages.pipe_metrics_json(key),
                None,
                None,
                lifecycle_snapshots.get(key),
            ))
        })
        .collect();

    let transcoder_rings: Vec<serde_json::Value> = stages
        .runtimes
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
        stages.runtimes.len(),
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
    // One registry at a time (see `StageMaps`).
    let (ingest, has_ingest) = {
        let ingests = engine.ingests.active.read().await;
        let ingest = ingests.get(pipeline_id);
        (
            ingest.map(|ingest| api_view_models::pipeline_ingest_telemetry_json(ingest.as_ref())),
            ingest.is_some(),
        )
    };
    let ring_info = engine
        .ingests
        .pipelines
        .read()
        .await
        .get(pipeline_id)
        .map(|ring| api_view_models::pipeline_source_ring_json(ring));
    let (pipeline_egresses, delivery): (Vec<serde_json::Value>, _) = {
        let egresses = engine.egresses.active.read().await;
        let pipeline_egresses = || {
            egresses
                .values()
                .filter(|egress| egress.pipeline_id == pipeline_id)
        };
        (
            pipeline_egresses()
                .map(|egress| api_view_models::egress_runtime_json(egress, true, has_ingest, None))
                .collect(),
            feed_delivery(pipeline_egresses().map(delivery_input)),
        )
    };
    let stage_maps = StageMaps::read(engine, Some(pipeline_id)).await;
    let stage_keys = stage_maps.keys();
    let lifecycle_snapshots = lifecycle_snapshots(engine, &stage_keys).await;

    let mut stages = Vec::with_capacity(stage_keys.len());
    for key in &stage_keys {
        let Some(metrics) = stage_maps.metrics_for(key) else {
            continue;
        };
        let mut val = api_view_models::stage_telemetry_row_json(
            key,
            serde_json::to_value(metrics.snapshot()).unwrap_or_default(),
            stage_maps.pipe_metrics_json(key),
            None,
            None,
            lifecycle_snapshots.get(key),
        );
        if let Some(runtime) = stage_maps.runtimes.get(key) {
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
    let stages = StageMaps::read(engine, None).await;
    let key = stages
        .keys()
        .into_iter()
        .find(|key| key.to_string() == display)?;
    let metrics = stages.metrics_for(&key)?;
    let pipe = stages.pipe_metrics_json(&key);

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

    /// A telemetry view parked on one stage map while another pipeline's
    /// stage start (a `runtimes` writer) and an SRT publisher's registration
    /// (an `ingests.active` writer, under its pipeline's selection) run: all three
    /// must finish. The test holds `stages.metrics` for writing, as an
    /// in-flight metrics writer would, so the view parks at that read. A
    /// view that holds nothing there lets both writers through at once. One
    /// that kept its earlier guards blocks them, and once released it reads
    /// `runtimes` again behind the queued stage-start writer, which waits on
    /// the view: a deadlock that also froze every ingest registration.
    async fn assert_view_never_deadlocks_stage_and_ingest_writers<F, Fut>(view: F)
    where
        F: FnOnce(Arc<MediaEngine>) -> Fut,
        Fut: Future<Output = serde_json::Value> + Send + 'static,
    {
        use restream_engine::stage_lifecycle::StageBackendKind;
        use tokio_util::sync::CancellationToken;

        let engine = Arc::new(MediaEngine::new());
        let start_stage = |pipeline: &str| {
            let engine = engine.clone();
            let key = StageKey::new(pipeline, StageKind::hls());
            async move {
                engine
                    .get_or_create_non_ring_stage_runtime(
                        key,
                        StagePhase::Registered,
                        StageBackendKind::HlsSegmenter,
                        CancellationToken::new(),
                    )
                    .await;
            }
        };
        start_stage("pipe-1").await;

        let metrics_writer = engine.stages.metrics.write().await;
        let view = tokio::spawn(view(engine.clone()));
        // Current-thread runtime: each yield runs the spawned tasks until
        // they park on a lock.
        tokio::task::yield_now().await;
        let stage_start = tokio::spawn(start_stage("pipe-2"));
        let ingest_engine = engine.clone();
        let publisher = tokio::spawn(async move {
            ingest_engine
                .try_register_ingest("pipe-2", "key-2", "srt")
                .await
        });
        tokio::task::yield_now().await;
        drop(metrics_writer);

        let (view, stage_start, publisher) =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(view, stage_start, publisher)
            })
            .await
            .expect("telemetry view, stage start and ingest registration deadlocked");
        assert!(view.expect("view task").is_object());
        stage_start.expect("stage start task");
        let lease = publisher
            .expect("registration task")
            .expect("the publisher on pipe-2 registers");
        drop(lease);
    }

    #[tokio::test]
    async fn engine_telemetry_never_deadlocks_stage_and_ingest_writers() {
        assert_view_never_deadlocks_stage_and_ingest_writers(|engine| async move {
            engine_telemetry(&engine).await
        })
        .await;
    }

    #[tokio::test]
    async fn pipeline_telemetry_never_deadlocks_stage_and_ingest_writers() {
        assert_view_never_deadlocks_stage_and_ingest_writers(|engine| async move {
            pipeline_telemetry(&engine, "pipe-1").await
        })
        .await;
    }
}
