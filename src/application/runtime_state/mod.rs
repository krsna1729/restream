//! One revisioned runtime state (Q-042).
//!
//! Each pipeline and output is projected into one typed record that holds
//! the operator's intent (desired), what the engine is doing (actual), how
//! the two relate (convergence), whether media moves (flow), queue pressure
//! the latest failure evidence, and the diagnosis of a degraded output
//! ([`crate::application::diagnosis`]). Each pipeline input carries its
//! stored settings and its session's state ([`inputs`]), and each pipeline
//! its processing graph ([`graph`]). Only state is projected: counters that
//! advance on every read (rates, bytes, fill, lag, transport quality) are
//! sampled from `/api/v1/engine/telemetry` instead, or every rebuild would
//! be a delta. [`hub::StateHub`] keeps the current projection under a
//! monotonically increasing revision and publishes the entity-level
//! difference of each change as a [`StateDelta`], so a client fetches
//! `/api/v1/state` once and then applies deltas from
//! `/api/v1/state/stream`; a client that misses deltas resyncs from a full
//! snapshot.
//!
//! The TypeScript contract for these types is generated from them
//! (`web/ts/core/generated/runtime-state.ts`, see `tests.rs`).

#[cfg(test)]
#[path = "entity_tests.rs"]
mod entity_tests;
pub mod graph;
pub mod hub;
pub mod inputs;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;

use serde::Serialize;

use crate::application::diagnosis::{Diagnosis, OutputSignals, ShardHealth, diagnose};
use crate::application::models::Output;
use crate::domain::output_spec::EgressProtocol;
use crate::domain::state::{DesiredOutputState, EgressPhase};
use crate::media::engine::MediaEngine;

pub use graph::{GraphEdge, GraphNode, PipelineGraph};
pub use inputs::{IngestEndpoints, IngestUrls, InputState};

/// The whole runtime state at one revision.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RuntimeState {
    /// Identifies this server run; revisions restart with each run, so a
    /// resume token is `<epoch>.<revision>`.
    pub epoch: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub revision: u64,
    pub pipelines: Vec<PipelineState>,
    pub outputs: Vec<OutputState>,
    pub inputs: Vec<InputState>,
    /// One per pipeline.
    pub graphs: Vec<PipelineGraph>,
}

/// What changed between revision `revision - 1` and `revision`: entities to
/// insert or replace whole, and ids that no longer exist.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct StateDelta {
    pub epoch: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub revision: u64,
    pub pipelines: Vec<PipelineState>,
    pub outputs: Vec<OutputState>,
    pub inputs: Vec<InputState>,
    pub graphs: Vec<PipelineGraph>,
    pub removed_pipelines: Vec<String>,
    pub removed_outputs: Vec<String>,
    pub removed_inputs: Vec<String>,
    /// Pipeline ids whose graph is gone (the pipeline was deleted).
    pub removed_graphs: Vec<String>,
}

impl StateDelta {
    fn is_empty(&self) -> bool {
        self.pipelines.is_empty()
            && self.outputs.is_empty()
            && self.inputs.is_empty()
            && self.graphs.is_empty()
            && self.removed_pipelines.is_empty()
            && self.removed_outputs.is_empty()
            && self.removed_inputs.is_empty()
            && self.removed_graphs.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct PipelineState {
    pub id: String,
    pub name: String,
    /// The selected live ingest, if one is connected.
    pub ingest: Option<IngestState>,
    /// Connected inputs held in warm standby (connected, not selected).
    pub standby_inputs: u32,
    /// A recording file is being written.
    pub recording: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct IngestState {
    pub input_id: String,
    pub protocol: String,
    pub video_codec: Option<String>,
    /// The ingest is an SRT group (bonded) connection.
    pub bonded: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct OutputState {
    pub id: String,
    pub pipeline_id: String,
    pub name: String,
    pub protocol: EgressProtocol,
    /// The egress shard carrying the running attempt; `null` off the fabric
    /// or when none runs.
    pub shard: Option<u32>,
    /// The last stage the running attempt reads (`<pipeline>:<kind>`), so
    /// outputs with the same stage share its work; `null` when none runs.
    pub stage: Option<String>,
    /// The operator's intent.
    pub desired: DesiredOutputState,
    /// The running egress attempt's phase; `null` when none runs.
    pub actual: Option<EgressPhase>,
    pub convergence: Convergence,
    pub flow: Flow,
    /// Why the egress is held back, when it is (`feedLag`, `socket`, ...).
    pub backpressure: Option<String>,
    pub retry: Option<RetryState>,
    pub evidence: Option<FailureEvidence>,
    /// Why a desired-running output is degraded; `null` when it is not.
    pub diagnosis: Option<Diagnosis>,
}

/// How actual relates to desired.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum Convergence {
    /// Actual matches desired: delivering, or stopped as asked.
    Converged,
    /// Desired running; an attempt is starting or will start on the next pass.
    Starting,
    /// Desired running; no live ingest to deliver.
    WaitingForIngest,
    /// Desired running; waiting out a retry backoff.
    Retrying,
    /// Desired running; the restart budget is spent (an edit, start or stop
    /// clears it).
    RetriesExhausted,
    /// Desired stopped; the egress is still ending.
    Stopping,
}

/// Whether media moves through the output.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum Flow {
    Flowing,
    /// Running but without progress for longer than the stall threshold.
    Stalled,
    Idle,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RetryState {
    pub attempts: u32,
    /// Unix epoch milliseconds of the next attempt.
    #[cfg_attr(test, ts(type = "number"))]
    pub next_attempt_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FailureEvidence {
    pub error: String,
    /// Where it failed (`connect`, `upload_segment`, ...), when known.
    pub phase: Option<String>,
    /// Unix epoch milliseconds.
    #[cfg_attr(test, ts(type = "number"))]
    pub at_ms: u64,
}

/// One projection of every entity, keyed by id (graphs by pipeline id).
#[derive(Debug, Default, PartialEq)]
pub struct Projection {
    pub pipelines: BTreeMap<String, PipelineState>,
    pub outputs: BTreeMap<String, OutputState>,
    pub inputs: BTreeMap<String, InputState>,
    pub graphs: BTreeMap<String, PipelineGraph>,
}

/// The stored rows a projection reads, and where inputs publish.
#[derive(Clone, Copy)]
pub struct Rows<'a> {
    pub pipelines: &'a [crate::db::PipelineRecord],
    pub outputs: &'a [Output],
    /// In list order: by pipeline, the primary first, backups by age.
    pub inputs: &'a [crate::domain::pipeline_input::PipelineInput],
    pub endpoints: &'a IngestEndpoints,
}

/// Projects every entity from its rows and the engine's runtime maps. Each
/// engine map is read once, so the projection is consistent per map and
/// costs one pass over the outputs.
pub async fn project(engine: &MediaEngine, rows: &Rows<'_>) -> Projection {
    let Rows {
        pipelines,
        outputs,
        inputs,
        endpoints,
    } = *rows;
    let ingests: HashMap<String, IngestState> = engine
        .ingests
        .active
        .read()
        .await
        .iter()
        .map(|(pipeline_id, ingest)| {
            let metadata = ingest.metadata();
            (
                pipeline_id.clone(),
                IngestState {
                    input_id: ingest.input_id.clone(),
                    protocol: ingest.protocol.clone(),
                    video_codec: metadata.video.map(|video| video.codec),
                    bonded: metadata.quality.srt_bonded == Some(true),
                },
            )
        })
        .collect();
    // Every connected input session (a configured file source too) that is
    // not its pipeline's selected one is a warm standby.
    let mut standby: HashMap<String, u32> = HashMap::new();
    for session in engine.ingests.sessions.read().await.values() {
        let selected = ingests
            .get(&session.pipeline_id)
            .is_some_and(|ingest| ingest.input_id == session.input_id);
        if !selected {
            *standby.entry(session.pipeline_id.clone()).or_default() += 1;
        }
    }
    let recording: std::collections::HashSet<String> = engine
        .recordings
        .cancel_tokens
        .read()
        .await
        .iter()
        .filter(|(_, token)| !token.is_cancelled())
        .map(|(pipeline_id, _)| pipeline_id.clone())
        .collect();
    let retries = engine.egresses.retry.read().await.clone();
    // Read one map at a time: no new lock nesting between egress maps.
    let recent_evidence: HashMap<String, FailureEvidence> = engine
        .egresses
        .recent
        .read()
        .await
        .iter()
        .filter_map(|(output_id, outcome)| {
            outcome.last_error.clone().map(|error| {
                (
                    output_id.clone(),
                    FailureEvidence {
                        error,
                        phase: outcome.failure_phase.clone(),
                        at_ms: outcome.last_error_ms,
                    },
                )
            })
        })
        .collect();
    let mut shards: HashMap<String, HashMap<u32, ShardHealth>> = HashMap::new();
    for status in engine
        .egress_fabric_shard_statuses(
            crate::media::engine_egress_fabric_diagnostics::SHARD_STALL_AFTER,
        )
        .await
    {
        use crate::media::egress::shard::EgressShardHealth as Health;
        let health = match status.state {
            Health::Healthy => ShardHealth::Healthy,
            Health::Stalled => ShardHealth::Stalled,
            Health::Stopped => ShardHealth::Stopped,
            Health::Panicked => ShardHealth::Panicked,
        };
        shards
            .entry(status.feed_id)
            .or_default()
            .insert(status.shard_index, health);
    }
    // Outputs share stages: look each terminal stage up once, with the
    // egress map released (the stage registries are separate locks).
    let terminal_keys: std::collections::HashSet<_> = engine
        .egresses
        .active
        .read()
        .await
        .values()
        .filter_map(|egress| egress.terminal_stage_key.clone())
        .collect();
    let mut blocked = HashMap::new();
    for key in terminal_keys {
        if let Some(stage) = engine.egress_blocked_by_stage_snapshot(&key).await {
            let phase = crate::runtime::stage::phase_name(&stage.phase).to_string();
            blocked.insert(key, (stage.key.to_string(), phase));
        }
    }
    let context = ProjectContext {
        shards: &shards,
        blocked: &blocked,
        max_retries: u64::from(engine.config.tuning.output_max_retries),
    };

    let mut projection = Projection::default();
    {
        let active = engine.egresses.active.read().await;
        for output in outputs {
            let state = project_output(
                output,
                active.get(&output.id),
                recent_evidence.get(&output.id),
                retries.get(&output.id),
                ingests.contains_key(&output.pipeline_id),
                &context,
            );
            projection.outputs.insert(output.id.clone(), state);
        }
    }
    for pipeline in pipelines {
        projection.pipelines.insert(
            pipeline.id.clone(),
            PipelineState {
                id: pipeline.id.clone(),
                name: pipeline.name.clone(),
                ingest: ingests.get(&pipeline.id).cloned(),
                standby_inputs: standby.get(&pipeline.id).copied().unwrap_or(0),
                recording: recording.contains(&pipeline.id),
            },
        );
    }
    projection.inputs = inputs::project_inputs(engine, inputs, endpoints).await;
    let pipeline_ids: Vec<&str> = pipelines
        .iter()
        .map(|pipeline| pipeline.id.as_str())
        .collect();
    let runtime = graph::GraphRuntime::read(engine, &pipeline_ids).await;
    let mut outputs_of: HashMap<&str, Vec<&Output>> = HashMap::new();
    for output in outputs {
        outputs_of
            .entry(output.pipeline_id.as_str())
            .or_default()
            .push(output);
    }
    for pipeline_id in pipeline_ids {
        let own = outputs_of.get(pipeline_id).map_or(&[][..], Vec::as_slice);
        projection.graphs.insert(
            pipeline_id.to_string(),
            graph::pipeline_graph(pipeline_id, own, &runtime),
        );
    }
    projection
}

/// Engine state read once per projection and shared by every output.
struct ProjectContext<'a> {
    /// Feed id -> shard index -> health.
    shards: &'a HashMap<String, HashMap<u32, ShardHealth>>,
    /// Terminal stages not producing: key -> (blocking stage, phase).
    blocked: &'a HashMap<crate::domain::stage::StageKey, (String, String)>,
    max_retries: u64,
}

fn project_output(
    output: &Output,
    active: Option<&crate::media::engine::ActiveEgress>,
    recent_evidence: Option<&FailureEvidence>,
    retry: Option<&crate::media::engine::EgressRetryState>,
    has_ingest: bool,
    context: &ProjectContext<'_>,
) -> OutputState {
    let actual = active.map(|egress| *crate::sync::lock(&egress.phase));
    // The same rule as the output status view: without ingest nothing is
    // expected to move, so an egress cannot be stalled.
    let stalled = active.is_some_and(|egress| {
        MediaEngine::egress_effective_status(egress, has_ingest) == "stalled"
    });
    let progressed =
        active.is_some_and(|egress| egress.last_progress_ms.load(Ordering::Relaxed) > 0);
    let delivering = matches!(
        actual,
        Some(
            EgressPhase::Sending
                | EgressPhase::Discarding
                | EgressPhase::Segmenting
                | EgressPhase::Uploading
        )
    );
    let retry = retry.map(|retry| RetryState {
        attempts: retry.attempts,
        next_attempt_at_ms: retry.next_retry_at_ms,
    });
    let convergence = match output.desired_state {
        DesiredOutputState::Stopped if active.is_some() => Convergence::Stopping,
        DesiredOutputState::Stopped => Convergence::Converged,
        DesiredOutputState::Running if output.retries_exhausted => Convergence::RetriesExhausted,
        // A stall is a flow problem (`flow: stalled`), not a convergence one.
        DesiredOutputState::Running if delivering => Convergence::Converged,
        DesiredOutputState::Running if active.is_some() => Convergence::Starting,
        DesiredOutputState::Running if retry.is_some() => Convergence::Retrying,
        DesiredOutputState::Running if !has_ingest => Convergence::WaitingForIngest,
        DesiredOutputState::Running => Convergence::Starting,
    };
    let flow = if stalled {
        Flow::Stalled
    } else if delivering && progressed && has_ingest {
        Flow::Flowing
    } else {
        Flow::Idle
    };
    let evidence = match active {
        Some(egress) => {
            crate::sync::lock(&egress.last_error)
                .clone()
                .map(|error| FailureEvidence {
                    error,
                    phase: crate::sync::lock(&egress.failure_phase).clone(),
                    at_ms: egress.last_error_ms.load(Ordering::Relaxed),
                })
        }
        None => recent_evidence.cloned(),
    };
    let backpressure = active
        .and_then(|egress| *crate::sync::lock(&egress.backpressure_reason))
        .map(str::to_string);
    let diagnosis = diagnose(&OutputSignals {
        wants_running: output.desired_state == DesiredOutputState::Running,
        retries_exhausted: output.retries_exhausted,
        has_ingest,
        blocked_by: active
            .and_then(|egress| egress.terminal_stage_key.as_ref())
            .and_then(|key| context.blocked.get(key))
            .cloned(),
        shard: active
            .and_then(|egress| egress.fabric.as_ref())
            .map(|slot| {
                let health = context
                    .shards
                    .get(slot.feed.as_str())
                    .and_then(|feed| feed.get(&slot.shard))
                    .copied()
                    // A fabric output whose shard reports no heartbeat yet is
                    // starting on it, not failing.
                    .unwrap_or(ShardHealth::Healthy);
                (slot.shard, health)
            }),
        failing: matches!(actual, Some(EgressPhase::Retrying | EgressPhase::Failed))
            || (active.is_none() && retry.is_some()),
        last_failure: evidence
            .as_ref()
            .map(|evidence| (evidence.phase.clone(), evidence.error.clone())),
        retry: retry
            .as_ref()
            .map(|retry| (retry.attempts, context.max_retries)),
        backpressure: backpressure.clone(),
        stalled,
    });
    OutputState {
        id: output.id.clone(),
        pipeline_id: output.pipeline_id.clone(),
        name: output.name.clone(),
        desired: output.desired_state,
        protocol: EgressProtocol::from_url(&output.url),
        shard: active
            .and_then(|egress| egress.fabric.as_ref())
            .map(|slot| slot.shard),
        stage: active
            .and_then(|egress| egress.terminal_stage_key.as_ref())
            .map(ToString::to_string),
        actual,
        convergence,
        flow,
        backpressure,
        retry,
        evidence,
        diagnosis,
    }
}

/// The entity-level difference from `old` to `new` (epoch and revision left
/// for the hub to set).
pub fn diff(old: &Projection, new: &Projection) -> StateDelta {
    fn changed<V: Clone + PartialEq>(
        old: &BTreeMap<String, V>,
        new: &BTreeMap<String, V>,
    ) -> (Vec<V>, Vec<String>) {
        let upserts = new
            .iter()
            .filter(|(id, value)| old.get(*id) != Some(value))
            .map(|(_, value)| value.clone())
            .collect();
        let removed = old
            .keys()
            .filter(|id| !new.contains_key(*id))
            .cloned()
            .collect();
        (upserts, removed)
    }
    let (pipelines, removed_pipelines) = changed(&old.pipelines, &new.pipelines);
    let (outputs, removed_outputs) = changed(&old.outputs, &new.outputs);
    let (inputs, removed_inputs) = changed(&old.inputs, &new.inputs);
    let (graphs, removed_graphs) = changed(&old.graphs, &new.graphs);
    StateDelta {
        epoch: String::new(),
        revision: 0,
        pipelines,
        outputs,
        inputs,
        graphs,
        removed_pipelines,
        removed_outputs,
        removed_inputs,
        removed_graphs,
    }
}
