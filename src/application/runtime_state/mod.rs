//! One revisioned runtime state.
//!
//! Each pipeline and output is projected into one typed record that holds
//! the operator's intent (desired), what the engine is doing (actual), how
//! the two relate (convergence), whether media moves (flow), queue pressure
//! the latest failure evidence, and the diagnosis of a degraded output
//! ([`restream_health::diagnosis`]). Each pipeline input carries its
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

use restream_domain::domain::output_spec::EgressProtocol;
use restream_domain::domain::state::{DesiredOutputState, EgressPhase};
use restream_domain::models::Output;
use restream_engine::engine::MediaEngine;
use restream_health::diagnosis::{Diagnosis, OutputSignals, ShardHealth, diagnose};

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
    /// Unix epoch ms since when this input has been the connected source.
    #[cfg_attr(test, ts(type = "number"))]
    pub since_ms: u64,
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
    /// Unix epoch ms since when the output has been in its current state
    /// (desired, convergence, flow, degraded or not): "live for 2 h", "down
    /// for 4 min". Outputs present when the engine started read from then.
    #[cfg_attr(test, ts(type = "number"))]
    pub state_since_ms: u64,
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
    pub inputs: &'a [restream_domain::domain::pipeline_input::PipelineInput],
    pub endpoints: &'a IngestEndpoints,
}

/// Projects every entity from its rows and the engine's runtime maps. Each
/// engine map is read once, so the projection is consistent per map and
/// costs one pass over the outputs.
pub async fn project(engine: &MediaEngine, rows: &Rows<'_>, output_max_retries: u32) -> Projection {
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
                    since_ms: 0,
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
            restream_engine::engine_egress_fabric_diagnostics::SHARD_STALL_AFTER,
        )
        .await
    {
        use restream_engine::egress::shard::EgressShardHealth as Health;
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
            let phase = restream_domain::runtime::stage::phase_name(&stage.phase).to_string();
            blocked.insert(key, (stage.key.to_string(), phase));
        }
    }
    let context = ProjectContext {
        shards: &shards,
        blocked: &blocked,
        max_retries: u64::from(output_max_retries),
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
    blocked: &'a HashMap<restream_domain::domain::stage::StageKey, (String, String)>,
    max_retries: u64,
}

fn project_output(
    output: &Output,
    active: Option<&restream_engine::engine::ActiveEgress>,
    recent_evidence: Option<&FailureEvidence>,
    retry: Option<&restream_engine::engine::EgressRetryState>,
    has_ingest: bool,
    context: &ProjectContext<'_>,
) -> OutputState {
    let actual = active.map(|egress| *restream_sync::lock::lock(&egress.phase));
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
        Some(egress) => restream_sync::lock::lock(&egress.last_error)
            .clone()
            .map(|error| FailureEvidence {
                error,
                phase: restream_sync::lock::lock(&egress.failure_phase).clone(),
                at_ms: egress.last_error_ms.load(Ordering::Relaxed),
            }),
        None => recent_evidence.cloned(),
    };
    let backpressure = active
        .and_then(|egress| *restream_sync::lock::lock(&egress.backpressure_reason))
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
        state_since_ms: 0,
    }
}

/// The entity-level difference from `old` to `new` (epoch and revision left
/// for the hub to set).
/// Stamps when each output entered its state and each ingest connected:
/// unchanged since `old`, the old stamp carries over; changed (or new), it
/// is `now_ms`. A new projection is built with zero stamps, so this runs on
/// every publish before the diff.
pub fn stamp_since(old: &Projection, new: &mut Projection, now_ms: u64) {
    for (id, output) in new.outputs.iter_mut() {
        let same = old.outputs.get(id).filter(|before| {
            before.desired == output.desired
                && before.convergence == output.convergence
                && before.flow == output.flow
                && before.diagnosis.is_some() == output.diagnosis.is_some()
        });
        output.state_since_ms = same.map_or(now_ms, |before| before.state_since_ms);
    }
    for (id, pipeline) in new.pipelines.iter_mut() {
        if let Some(ingest) = pipeline.ingest.as_mut() {
            let same = old
                .pipelines
                .get(id)
                .and_then(|before| before.ingest.as_ref())
                .filter(|before| before.input_id == ingest.input_id);
            ingest.since_ms = same.map_or(now_ms, |before| before.since_ms);
        }
    }
}

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
