//! API/runtime graph adapters for visualizing a pipeline's active processing
//! topology.
//! The structure (nodes, edges, labels, which nodes run) is the runtime
//! state's ([`crate::application::runtime_state::graph`]); this file adds
//! each node's details and counters for the HTTP view.

use super::graph_projection as api_view_models;
use crate::application::models::Output;
use crate::application::runtime_state::graph::{
    GraphNode, GraphRuntime, NodeSource, pipeline_graph,
};
use crate::domain::stage::StageKey;
use crate::media::engine::MediaEngine;
use std::collections::HashMap;

pub(crate) async fn processing_graph(
    engine: &MediaEngine,
    pipeline_id: &str,
    outputs: &[Output],
) -> serde_json::Value {
    let pipeline_outputs: Vec<&Output> = outputs
        .iter()
        .filter(|output| output.pipeline_id == pipeline_id)
        .collect();
    let runtime = GraphRuntime::read(engine, &[pipeline_id]).await;
    let graph = pipeline_graph(pipeline_id, &pipeline_outputs, &runtime);
    let lifecycle_snapshots: HashMap<StageKey, crate::runtime::stage::StageRuntimeSnapshot> =
        engine
            .pipeline_stage_runtime_snapshots(pipeline_id)
            .await
            .into_iter()
            .map(|snapshot| (snapshot.key.clone(), snapshot))
            .collect();

    // Each registry map is read into owned data and released before the
    // next is taken, never nested: a reader holding one map while waiting
    // on another deadlocks against a writer that takes them in the other
    // order (`release_stage_runtime` holds `runtimes`, then takes `metrics`),
    // and tokio's fair lock then queues every later reader behind it.
    let ingest = engine.ingests.active.read().await.get(pipeline_id).cloned();
    let ring = engine
        .ingests
        .pipelines
        .read()
        .await
        .get(pipeline_id)
        .cloned();
    let egresses: HashMap<&str, (serde_json::Value, serde_json::Value)> = {
        let active = engine.egresses.active.read().await;
        graph
            .nodes
            .iter()
            .filter_map(|node| node.output_id.as_deref())
            .filter_map(|output_id| {
                let egress = active.get(output_id)?;
                Some((
                    output_id,
                    (
                        api_view_models::processing_graph_egress_details(egress, ingest.is_some()),
                        serde_json::to_value(egress.metrics.snapshot()).unwrap_or_default(),
                    ),
                ))
            })
            .collect()
    };
    let output_stage_metrics: HashMap<&StageKey, serde_json::Value> = {
        let metrics = engine.stages.metrics.read().await;
        graph
            .nodes
            .iter()
            .filter_map(|node| match &node.source {
                NodeSource::Recording(key) | NodeSource::Hls(key) => metrics
                    .get(key)
                    .map(|m| (key, serde_json::to_value(m.snapshot()).unwrap_or_default())),
                _ => None,
            })
            .collect()
    };
    let packetizer_payloads: HashMap<&str, serde_json::Value> = {
        let ts_muxers = engine.stages.ts_muxers.read().await;
        graph
            .nodes
            .iter()
            .filter_map(|node| match &node.source {
                NodeSource::Packetizer { key, .. } => ts_muxers.get(key.as_str()).map(|stage| {
                    (
                        key.as_str(),
                        api_view_models::ring_payload_stats_json(&stage.muxer.ring),
                    )
                }),
                _ => None,
            })
            .collect()
    };
    /// What a stage node shows of its running stage.
    struct StageView {
        metrics: serde_json::Value,
        pipe: Option<serde_json::Value>,
        payload: Option<serde_json::Value>,
    }
    let stage_views: HashMap<&StageKey, StageView> = {
        let runtimes = engine.stages.runtimes.read().await;
        graph
            .nodes
            .iter()
            .filter_map(|node| match &node.source {
                NodeSource::Stage(key) => runtimes.get(key).map(|stage| {
                    let view = StageView {
                        metrics: serde_json::to_value(stage.metrics.snapshot()).unwrap_or_default(),
                        pipe: stage
                            .pipe_metrics
                            .as_ref()
                            .map(|pipe| serde_json::to_value(pipe.snapshot()).unwrap_or_default()),
                        payload: stage
                            .ring
                            .as_ref()
                            .map(|ring| api_view_models::ring_payload_stats_json(ring)),
                    };
                    (key, view)
                }),
                _ => None,
            })
            .collect()
    };

    let ingest = ingest.as_ref();
    let ring = ring.as_ref();
    let reader_overflows_by_stage: HashMap<String, usize> = ring
        .map(|ring| ring.reader_snapshots())
        .unwrap_or_default()
        .into_iter()
        .filter(|reader| reader.overflow_count > 0)
        .map(|reader| (reader.name, reader.overflow_count))
        .collect();

    let nodes: Vec<serde_json::Value> = graph
        .nodes
        .iter()
        .map(|node| {
            let GraphNode {
                id,
                kind,
                label,
                active,
                ..
            } = node;
            let plain = |details, metrics| {
                api_view_models::processing_graph_node(
                    id.clone(),
                    kind.clone(),
                    label.clone(),
                    *active,
                    details,
                    metrics,
                )
            };
            let ingest_metrics = || {
                ingest.map(|ingest| {
                    serde_json::to_value(ingest.metrics.snapshot()).unwrap_or_default()
                })
            };
            match &node.source {
                NodeSource::Ingest => plain(
                    ingest.map(|ingest| {
                        api_view_models::processing_graph_ingest_details(ingest.as_ref())
                    }),
                    ingest_metrics(),
                ),
                NodeSource::Demux => plain(
                    ingest.map(|ingest| {
                        api_view_models::processing_graph_demux_details(ingest.as_ref())
                    }),
                    ingest_metrics(),
                ),
                NodeSource::SourceRing => plain(
                    ring.map(|ring| {
                        let (fill, cap) = ring.fill_and_capacity();
                        api_view_models::processing_graph_source_ring_details(
                            fill,
                            cap,
                            api_view_models::ring_payload_stats_json(ring),
                            MediaEngine::source_buffer_format(runtime.ingest_protocol(pipeline_id)),
                            ring.reader_snapshots()
                                .iter()
                                .map(api_view_models::reader_snapshot_json)
                                .collect(),
                        )
                    }),
                    None,
                ),
                NodeSource::Stage(key) => {
                    let stage = stage_views.get(key);
                    let stage_key = key.kind.to_string();
                    let mut value = api_view_models::processing_graph_stage_node(
                        id.clone(),
                        kind.clone(),
                        label.clone(),
                        stage_key.as_str(),
                        lifecycle_snapshots.get(key),
                        *active,
                        stage.map(|stage| stage.metrics.clone()),
                        stage.and_then(|stage| stage.pipe.clone()),
                        stage
                            .and_then(|stage| stage.payload.clone())
                            .unwrap_or_default(),
                    );
                    if let Some(overflow_count) = reader_overflows_by_stage.get(&stage_key)
                        && let Some(details) = value
                            .get_mut("details")
                            .and_then(serde_json::Value::as_object_mut)
                    {
                        details.insert("healthStatus".to_string(), serde_json::json!("warning"));
                        details.insert(
                            "healthReason".to_string(),
                            serde_json::json!(format!(
                                "source reader overflowed {overflow_count} time(s)"
                            )),
                        );
                        details.insert(
                            "overflowCount".to_string(),
                            serde_json::json!(overflow_count),
                        );
                    }
                    value
                }
                NodeSource::Packetizer { key, encoding } => plain(
                    Some(api_view_models::processing_graph_packetizer_details(
                        "srt",
                        encoding,
                        key.clone(),
                        packetizer_payloads.get(key.as_str()).cloned(),
                    )),
                    None,
                ),
                NodeSource::Egress => {
                    let egress = node
                        .output_id
                        .as_deref()
                        .and_then(|output| egresses.get(output));
                    let mut value = plain(
                        egress.map(|(details, _)| details.clone()),
                        egress.map(|(_, metrics)| metrics.clone()),
                    );
                    // Clients join egress nodes to outputs on this, running or not.
                    value["outputId"] = serde_json::json!(node.output_id);
                    value
                }
                NodeSource::Recirculation(target) => plain(
                    Some(
                        api_view_models::processing_graph_recirculation_target_details(
                            target.pipeline_id(),
                            target.input_id(),
                        ),
                    ),
                    None,
                ),
                NodeSource::Recording(key) | NodeSource::Hls(key) => {
                    plain(None, output_stage_metrics.get(key).cloned())
                }
            }
        })
        .collect();
    let edges = graph
        .edges
        .iter()
        .map(|edge| {
            api_view_models::processing_graph_edge(
                edge.from.clone(),
                edge.to.clone(),
                edge.label.clone(),
            )
        })
        .collect();

    api_view_models::processing_graph_json(
        chrono::Utc::now().to_rfc3339(),
        pipeline_id,
        nodes,
        edges,
    )
}
