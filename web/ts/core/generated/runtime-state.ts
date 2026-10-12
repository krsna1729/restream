// Generated from src/application/runtime_state (ts-rs). Do not edit.
// Regenerate: UPDATE_GENERATED=1 cargo test --lib runtime_state::tests::generated

export type RuntimeState = { 
/**
 * Identifies this server run; revisions restart with each run, so a
 * resume token is `<epoch>.<revision>`.
 */
epoch: string, revision: number, pipelines: Array<PipelineState>, outputs: Array<OutputState>, };

export type StateDelta = { epoch: string, revision: number, pipelines: Array<PipelineState>, outputs: Array<OutputState>, removedPipelines: Array<string>, removedOutputs: Array<string>, };

export type PipelineState = { id: string, name: string, 
/**
 * The selected live ingest, if one is connected.
 */
ingest: IngestState | null, };

export type IngestState = { inputId: string, protocol: string, videoCodec: string | null, };

export type OutputState = { id: string, pipelineId: string, name: string, 
/**
 * The operator's intent.
 */
desired: DesiredOutputState, 
/**
 * The running egress attempt's phase; `null` when none runs.
 */
actual: EgressPhase | null, convergence: Convergence, flow: Flow, 
/**
 * Why the egress is held back, when it is (`feedLag`, `socket`, ...).
 */
backpressure: string | null, retry: RetryState | null, evidence: FailureEvidence | null, };

export type Convergence = "converged" | "starting" | "waitingForIngest" | "retrying" | "retriesExhausted" | "stopping";

export type Flow = "flowing" | "stalled" | "idle";

export type RetryState = { attempts: number, 
/**
 * Unix epoch milliseconds of the next attempt.
 */
nextAttemptAtMs: number, };

export type FailureEvidence = { error: string, 
/**
 * Where it failed (`connect`, `upload_segment`, ...), when known.
 */
phase: string | null, 
/**
 * Unix epoch milliseconds.
 */
atMs: number, };

export type DesiredOutputState = "running" | "stopped";

export type EgressPhase = "starting" | "waitingUpstream" | "connecting" | "sending" | "discarding" | "segmenting" | "uploading" | "retrying" | "failed" | "stopped";
