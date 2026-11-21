//! Each pipeline's processing graph as runtime state: which nodes run, what
//! they are called and how media flows between them. Counters (packet
//! rates, ring fill, feed lag) are not state: `/api/v1/engine/telemetry`
//! samples them, and `GET /api/v1/pipelines/{id}/graph` attaches them to
//! this same structure ([`crate::api::runtime_views::processing_graph`]).

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use restream_domain::domain::output_spec::{RecirculationTarget, VideoCodecKind};
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_domain::models::Output;
use restream_domain::planner::{PlannedOutput, plan_pipeline_graph, plan_recording_graph};
use restream_engine::engine::MediaEngine;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PipelineGraph {
    pub pipeline_id: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct GraphNode {
    pub id: String,
    /// `ingest`, `demux`, `ring_buffer`, a stage's type, `packetizer`,
    /// `egress`, `recirculation_input`, `recording` or `hls`.
    #[serde(rename = "type")]
    #[cfg_attr(test, ts(rename = "type"))]
    pub kind: String,
    pub label: String,
    pub active: bool,
    /// On transcoder stage nodes: the stage (`<kind>` within the pipeline).
    pub stage_key: Option<String>,
    /// On `egress` nodes: the output the node sends for.
    pub output_id: Option<String>,
    /// The `stageKey` of the node's row in the engine telemetry's `stages`,
    /// where its counters are; `null` for nodes counted elsewhere (ingest
    /// and demux in `ingests`, the ring in `sourceRings`, egress in
    /// `egresses`).
    pub metrics_stage: Option<String>,
    /// What the node stands for in the engine, for the HTTP view's details.
    #[serde(skip)]
    #[cfg_attr(test, ts(skip))]
    pub(crate) source: NodeSource,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// The engine object behind a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NodeSource {
    Ingest,
    Demux,
    SourceRing,
    Stage(StageKey),
    /// A TS muxer, by its registry key, and the encoding it packetizes.
    Packetizer {
        key: String,
        encoding: String,
    },
    Egress,
    Recirculation(RecirculationTarget),
    Recording(StageKey),
    Hls(StageKey),
}

/// The live ingest, as the graph reads it.
struct GraphIngest {
    protocol: String,
    video_codec: Option<String>,
}

/// The engine state the graphs read, gathered one map at a time (no lock is
/// held while another is taken).
#[derive(Default)]
pub(crate) struct GraphRuntime {
    /// Pipeline -> its live ingest.
    ingests: HashMap<String, GraphIngest>,
    /// Pipelines with a source ring.
    rings: HashSet<String>,
    /// Output -> its egress counts as running (the output status rule).
    egresses: HashMap<String, bool>,
    /// Transcoder stages with a ring, sorted by key, with whether each runs.
    stages: Vec<(StageKey, bool)>,
    /// TS muxer registry key -> running.
    ts_muxers: HashMap<String, bool>,
    /// Pipeline -> its recording runs.
    recordings: HashMap<String, bool>,
    /// Pipeline -> its HLS preview runs, for pipelines with a preview store.
    hls: HashMap<String, bool>,
}

impl GraphRuntime {
    /// Reads what the graphs of `pipeline_ids` need.
    pub(crate) async fn read(engine: &MediaEngine, pipeline_ids: &[&str]) -> Self {
        let ingests: HashMap<String, GraphIngest> = engine
            .ingests
            .active
            .read()
            .await
            .iter()
            .map(|(pipeline_id, ingest)| {
                (
                    pipeline_id.clone(),
                    GraphIngest {
                        protocol: ingest.protocol.clone(),
                        video_codec: ingest.metadata().video.map(|video| video.codec),
                    },
                )
            })
            .collect();
        let rings = engine
            .ingests
            .pipelines
            .read()
            .await
            .keys()
            .cloned()
            .collect();
        let egresses = engine
            .egresses
            .active
            .read()
            .await
            .iter()
            .map(|(output_id, egress)| {
                let has_ingest = ingests.contains_key(&egress.pipeline_id);
                let running = MediaEngine::egress_effective_status(egress, has_ingest) == "running";
                (output_id.clone(), running)
            })
            .collect();
        let mut stages: Vec<(StageKey, bool)> = engine
            .stages
            .runtimes
            .read()
            .await
            .iter()
            .filter(|(_, runtime)| runtime.ring.is_some())
            .map(|(key, runtime)| (key.clone(), !runtime.cancel.is_cancelled()))
            .collect();
        stages.sort_by_cached_key(|(key, _)| key.to_string());
        let ts_muxers = engine
            .stages
            .ts_muxers
            .read()
            .await
            .iter()
            .map(|(key, stage)| (key.clone(), !stage.muxer.cancel.is_cancelled()))
            .collect();
        let recordings = engine
            .recordings
            .cancel_tokens
            .read()
            .await
            .iter()
            .map(|(pipeline_id, token)| (pipeline_id.clone(), !token.is_cancelled()))
            .collect();
        let mut hls = HashMap::new();
        for pipeline_id in pipeline_ids {
            let snapshot = engine.hls_dependency_snapshot(pipeline_id).await;
            if snapshot.store_exists {
                hls.insert((*pipeline_id).to_string(), snapshot.active);
            }
        }
        Self {
            ingests,
            rings,
            egresses,
            stages,
            ts_muxers,
            recordings,
            hls,
        }
    }

    /// The live ingest's protocol, for the HTTP view's source-ring format.
    pub(crate) fn ingest_protocol(&self, pipeline_id: &str) -> Option<&str> {
        self.ingests
            .get(pipeline_id)
            .map(|ingest| ingest.protocol.as_str())
    }
}

fn node(
    id: String,
    kind: &str,
    label: impl Into<String>,
    active: bool,
    source: NodeSource,
) -> GraphNode {
    GraphNode {
        id,
        kind: kind.to_string(),
        label: label.into(),
        active,
        stage_key: None,
        output_id: None,
        metrics_stage: None,
        source,
    }
}

fn edge(from: impl Into<String>, to: impl Into<String>, label: impl Into<String>) -> GraphEdge {
    GraphEdge {
        from: from.into(),
        to: to.into(),
        label: label.into(),
    }
}

/// The graph of one pipeline: its ingest, demux and source ring; the stages
/// the planner keeps for its wanted or running outputs (a stage left over
/// from an earlier plan is not drawn); one egress node per output, through
/// a TS mux for SRT; recirculation targets; recording and HLS preview.
/// `outputs` are the pipeline's own.
pub(crate) fn pipeline_graph(
    pipeline_id: &str,
    outputs: &[&Output],
    runtime: &GraphRuntime,
) -> PipelineGraph {
    let mut graph = PipelineGraph {
        pipeline_id: pipeline_id.to_string(),
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    let ingest = runtime.ingests.get(pipeline_id);
    let rb_node_id = push_source_chain(&mut graph, ingest, runtime);
    let ingest_video_codec = ingest.and_then(|ingest| ingest.video_codec.as_deref());
    let visible = visible_stage_keys(
        pipeline_id,
        outputs,
        runtime,
        ingest.is_some(),
        ingest_video_codec,
    );
    push_stages(&mut graph, runtime, &visible, &rb_node_id);

    let ingest_is_hevc =
        ingest_video_codec.is_some_and(|codec| VideoCodecKind::from_codec_name(codec).is_hevc());
    let mut added = AddedNodes::default();
    for output in outputs {
        push_output(
            &mut graph,
            output,
            runtime,
            &rb_node_id,
            ingest_is_hevc,
            &mut added,
        );
    }
    push_taps(&mut graph, runtime, &rb_node_id);
    graph
}

/// Ingest, demux and source ring with the edges between them. Returns the
/// source ring's node id.
fn push_source_chain(
    graph: &mut PipelineGraph,
    ingest: Option<&GraphIngest>,
    runtime: &GraphRuntime,
) -> String {
    let pipeline_id = graph.pipeline_id.clone();
    let ingest_node_id = format!("{pipeline_id}_ingest");
    graph.nodes.push(node(
        ingest_node_id.clone(),
        "ingest",
        ingest
            .map(|ingest| format!("{} ingest", ingest.protocol.to_uppercase()))
            .unwrap_or_else(|| "No ingest".to_string()),
        ingest.is_some(),
        NodeSource::Ingest,
    ));
    let demux_node_id = format!("{pipeline_id}_ingest_demux");
    graph.nodes.push(node(
        demux_node_id.clone(),
        "demux",
        ingest
            .map(|ingest| {
                format!(
                    "{} demux/probe",
                    MediaEngine::graph_protocol_label(&ingest.protocol)
                )
            })
            .unwrap_or_else(|| "Demux/probe idle".to_string()),
        ingest.is_some(),
        NodeSource::Demux,
    ));
    let rb_node_id = format!("{pipeline_id}_source_rb");
    graph.nodes.push(node(
        rb_node_id.clone(),
        "ring_buffer",
        "Source Buffer",
        runtime.rings.contains(&pipeline_id),
        NodeSource::SourceRing,
    ));
    graph.edges.push(edge(
        ingest_node_id,
        demux_node_id.clone(),
        ingest
            .map(|ingest| MediaEngine::graph_protocol_label(&ingest.protocol))
            .unwrap_or_else(|| "input".to_string()),
    ));
    graph
        .edges
        .push(edge(demux_node_id, rb_node_id.clone(), "push(MediaPacket)"));
    rb_node_id
}

/// The stages the planner keeps for the outputs that want to run or are
/// running, while an ingest is live.
fn visible_stage_keys(
    pipeline_id: &str,
    outputs: &[&Output],
    runtime: &GraphRuntime,
    has_ingest: bool,
    ingest_video_codec: Option<&str>,
) -> HashSet<StageKey> {
    let visible_outputs = outputs
        .iter()
        .filter(|output| {
            has_ingest && (output.wants_running() || runtime.egresses.contains_key(&output.id))
        })
        .map(|output| {
            PlannedOutput::new(
                output.id.as_str(),
                output.config.clone(),
                output.url.as_str(),
            )
        })
        .collect::<Vec<_>>();
    plan_pipeline_graph(pipeline_id, ingest_video_codec, &visible_outputs, false)
        .stages
        .into_iter()
        .filter(|stage| stage.kind != StageKind::Source)
        .map(|stage| stage.key)
        .collect()
}

fn push_stages(
    graph: &mut PipelineGraph,
    runtime: &GraphRuntime,
    visible: &HashSet<StageKey>,
    rb_node_id: &str,
) {
    let pipeline_id = graph.pipeline_id.clone();
    for (key, active) in &runtime.stages {
        if key.pipeline.as_str() != pipeline_id || !visible.contains(key) {
            continue;
        }
        let kind = &key.kind;
        let stage_id = kind.graph_node_id(&pipeline_id);
        let mut stage = node(
            stage_id.clone(),
            kind.graph_type(),
            kind.graph_label(),
            *active,
            NodeSource::Stage(key.clone()),
        );
        stage.stage_key = Some(kind.to_string());
        stage.metrics_stage = Some(key.to_string());
        graph.nodes.push(stage);
        if let Some((from, label)) = stage_input_edge(kind, &pipeline_id, rb_node_id) {
            graph.edges.push(edge(from, stage_id, label));
        }
    }
}

/// Where a stage reads from, and the edge's label.
fn stage_input_edge(
    kind: &StageKind,
    pipeline_id: &str,
    rb_node_id: &str,
) -> Option<(String, String)> {
    let codec_edge = matches!(kind, StageKind::CodecEdge { .. });
    match kind.upstream() {
        Some(StageKind::Source) => {
            let label = if codec_edge {
                "codec conversion"
            } else {
                "audio select"
            };
            Some((rb_node_id.to_string(), label.to_string()))
        }
        Some(upstream) => {
            let label = if codec_edge {
                "codec conversion"
            } else {
                "video copy + audio select"
            };
            Some((upstream.graph_node_id(pipeline_id), label.to_string()))
        }
        None => match kind {
            StageKind::VideoPreset { preset, .. } => {
                Some((rb_node_id.to_string(), format!("decode → {preset} encode")))
            }
            _ => None,
        },
    }
}

/// Shared nodes already drawn: one TS mux per encoding, one node per
/// recirculation target.
#[derive(Default)]
struct AddedNodes {
    packetizers: HashSet<String>,
    recirculation_targets: HashSet<String>,
}

/// One output's egress node, fed from its terminal stage (through a TS mux
/// for SRT), and its recirculation target, if any.
fn push_output(
    graph: &mut PipelineGraph,
    output: &Output,
    runtime: &GraphRuntime,
    rb_node_id: &str,
    ingest_is_hevc: bool,
    added: &mut AddedNodes,
) {
    let pipeline_id = graph.pipeline_id.clone();
    let output_node_id = format!("{pipeline_id}_output_{}", output.id);
    let protocol = MediaEngine::egress_protocol_from_url(&output.url);
    let protocol_label = MediaEngine::graph_protocol_label(protocol);
    let mut egress = node(
        output_node_id.clone(),
        "egress",
        format!("{protocol_label} sender: {}", output.name.as_str()),
        runtime.egresses.get(&output.id).copied().unwrap_or(false),
        NodeSource::Egress,
    );
    // Clients join egress nodes to outputs on this, running or not.
    egress.output_id = Some(output.id.clone());
    graph.nodes.push(egress);

    let output_for_plan = [PlannedOutput::new(
        output.id.as_str(),
        output.config.clone(),
        output.url.as_str(),
    )];
    let terminal_kind = plan_pipeline_graph(
        &pipeline_id,
        ingest_is_hevc.then_some("hevc"),
        &output_for_plan,
        false,
    )
    .terminal_stage
    .kind;
    let terminal_node_id = if matches!(terminal_kind, StageKind::Source) {
        rb_node_id.to_string()
    } else {
        terminal_kind.graph_node_id(&pipeline_id)
    };

    if protocol == "srt" {
        let encoding = output.stage_encoding_label();
        let mux_slug = MediaEngine::graph_slug(encoding.as_str());
        let mux_node_id = format!(
            "{pipeline_id}_ts_mux_{}",
            if mux_slug.is_empty() {
                "source"
            } else {
                mux_slug.as_str()
            }
        );
        if added.packetizers.insert(mux_node_id.clone()) {
            let key = format!("{pipeline_id}:{encoding}");
            graph.nodes.push(node(
                mux_node_id.clone(),
                "packetizer",
                format!("MPEG-TS mux: {encoding}"),
                runtime.ts_muxers.get(&key).copied().unwrap_or(false),
                NodeSource::Packetizer { key, encoding },
            ));
            graph
                .edges
                .push(edge(terminal_node_id, mux_node_id.clone(), "media packets"));
        }
        graph
            .edges
            .push(edge(mux_node_id, output_node_id.clone(), "SRT send"));
    } else {
        graph.edges.push(edge(
            terminal_node_id,
            output_node_id.clone(),
            MediaEngine::source_to_egress_label(protocol),
        ));
    }

    if let Ok(target) = RecirculationTarget::parse(&output.url) {
        push_recirculation(graph, output_node_id, target, added);
    }
}

fn push_recirculation(
    graph: &mut PipelineGraph,
    output_node_id: String,
    target: RecirculationTarget,
    added: &mut AddedNodes,
) {
    let target_node_id = format!(
        "{}_input_{}",
        target.pipeline_id(),
        MediaEngine::graph_slug(target.input_id())
    );
    if added.recirculation_targets.insert(target_node_id.clone()) {
        graph.nodes.push(node(
            target_node_id.clone(),
            "recirculation_input",
            format!(
                "Recirculation input: {}/{}",
                target.pipeline_id(),
                target.input_id()
            ),
            false,
            NodeSource::Recirculation(target),
        ));
    }
    graph.edges.push(edge(
        output_node_id,
        target_node_id,
        "in-process recirculation",
    ));
}

/// Recording and HLS preview, both reading the source ring.
fn push_taps(graph: &mut PipelineGraph, runtime: &GraphRuntime, rb_node_id: &str) {
    let pipeline_id = graph.pipeline_id.clone();
    if let Some(active) = runtime.recordings.get(&pipeline_id) {
        let key = plan_recording_graph(&pipeline_id).terminal_stage;
        let rec_id = format!("{pipeline_id}_recording");
        let mut recording = node(
            rec_id.clone(),
            "recording",
            "MKV Recording",
            *active,
            NodeSource::Recording(key.clone()),
        );
        recording.metrics_stage = Some(key.to_string());
        graph.nodes.push(recording);
        graph.edges.push(edge(rb_node_id, rec_id, "MKV mux"));
    }

    if let Some(active) = runtime.hls.get(&pipeline_id) {
        let key = StageKey::new(pipeline_id.as_str(), StageKind::hls());
        let hls_id = format!("{pipeline_id}_hls_preview");
        let mut hls = node(
            hls_id.clone(),
            "hls",
            "HLS Preview",
            *active,
            NodeSource::Hls(key.clone()),
        );
        hls.metrics_stage = Some(key.to_string());
        graph.nodes.push(hls);
        graph
            .edges
            .push(edge(rb_node_id, hls_id, "MPEG-TS segment"));
    }
}
