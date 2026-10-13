// Generated from src/application/runtime_state, diagnosis and capacity (ts-rs). Do not edit.
// Regenerate: cargo test --lib runtime_state::tests::write_generated -- --ignored

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

export type OutputState = { id: string, pipelineId: string, name: string, protocol: EgressProtocol, 
/**
 * The egress shard carrying the running attempt; `null` off the fabric
 * or when none runs.
 */
shard: number | null, 
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
backpressure: string | null, retry: RetryState | null, evidence: FailureEvidence | null, 
/**
 * Why a desired-running output is degraded; `null` when it is not.
 */
diagnosis: Diagnosis | null, };

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

export type Diagnosis = { layer: Layer, cause: Cause, 
/**
 * Layers checked and healthy, upstream of `layer`.
 */
ruledOut: Array<Layer>, };

export type Layer = "ingest" | "feed" | "shard" | "destination";

export type Cause = { "code": "noPublisher" } | { "code": "stageNotProducing", stage: string, phase: string, } | { "code": "shardPanicked", shard: number, } | { "code": "shardStalled", shard: number, } | { "code": "shardStopped", shard: number, } | { "code": "capacityRefused", error: string, } | { "code": "retriesExhausted" } | { "code": "retryBudgetNearlySpent", attempts: number, max: number, } | { "code": "rejected", phase: string | null, error: string, } | { "code": "backpressured", stalled: boolean, } | { "code": "noProgress" };

export type DesiredOutputState = "running" | "stopped";

export type EgressProtocol = "rtmp" | "srt" | "hls" | "sink" | "pipeline" | "unknown";

export type EgressPhase = "starting" | "waitingUpstream" | "connecting" | "sending" | "discarding" | "segmenting" | "uploading" | "retrying" | "failed" | "stopped";

export type CapacitySnapshot = { ingressPps: number, mediaBps: number, egressPps: number, hottestShardUtil: number, nicUtil: number, memoryUtil: number, ffmpegUtil: number, diskUtil: number, activeLeaves: number, uniqueStages: number, hottestCenter: ServiceCenter, projectedUtilization: number, flow: FlowDiagnosis, 
/**
 * `flow` comes from running egress shards; otherwise it is the
 * projection's hottest center, priced from the priors.
 */
flowMeasured: boolean, observeOnly: boolean, };

export type ServiceCenter = "ingress" | "egress" | "cpu" | "nic" | "memory" | "ffmpeg" | "disk";

export type FlowDiagnosis = { center: ServiceCenter, utilization: number, queue: number, backlogSlope: number, 
/**
 * Infinite (serialized `null`) when no deadline applies.
 */
deadlineSlackMs: number | null, delayMs: number, errors: number, amplification: number, status: FlowStatus, };

export type FlowStatus = "healthy" | "saturated" | "overloaded";
