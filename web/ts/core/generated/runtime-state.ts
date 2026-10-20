// Generated from src/application/runtime_state, diagnosis, capacity and fleet_history (ts-rs). Do not edit.
// Regenerate: cargo test --lib runtime_state::tests::write_generated -- --ignored

export type RuntimeState = { 
/**
 * Identifies this server run; revisions restart with each run, so a
 * resume token is `<epoch>.<revision>`.
 */
epoch: string, revision: number, pipelines: Array<PipelineState>, outputs: Array<OutputState>, inputs: Array<InputState>, 
/**
 * One per pipeline.
 */
graphs: Array<PipelineGraph>, };

export type StateDelta = { epoch: string, revision: number, pipelines: Array<PipelineState>, outputs: Array<OutputState>, inputs: Array<InputState>, graphs: Array<PipelineGraph>, removedPipelines: Array<string>, removedOutputs: Array<string>, removedInputs: Array<string>, 
/**
 * Pipeline ids whose graph is gone (the pipeline was deleted).
 */
removedGraphs: Array<string>, };

export type PipelineState = { id: string, name: string, 
/**
 * The selected live ingest, if one is connected.
 */
ingest: IngestState | null, 
/**
 * Connected inputs held in warm standby (connected, not selected).
 */
standbyInputs: number, 
/**
 * A recording file is being written.
 */
recording: boolean, };

export type IngestState = { inputId: string, protocol: string, videoCodec: string | null, 
/**
 * The ingest is an SRT group (bonded) connection.
 */
bonded: boolean, };

export type OutputState = { id: string, pipelineId: string, name: string, protocol: EgressProtocol, 
/**
 * The egress shard carrying the running attempt; `null` off the fabric
 * or when none runs.
 */
shard: number | null, 
/**
 * The last stage the running attempt reads (`<pipeline>:<kind>`), so
 * outputs with the same stage share its work; `null` when none runs.
 */
stage: string | null, 
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

export type InputState = { id: string, pipelineId: string, 
/**
 * Where the input sits in its pipeline's list: the primary first, then
 * the backups in the order they were added.
 */
position: number, label: string, streamKey: string, role: PipelineInputRole, enabled: boolean, selected: boolean, kind: PipelineInputSourceKind, 
/**
 * The remote origin, for a pull input only.
 */
sourceUrl: string | null, variantRule: VariantRule, 
/**
 * Auth is write-only: the variant and (for basic) the username are safe
 * to show, but no token, password or header value ever is.
 */
authKind: "none" | "bearer" | "cookies" | "basic" | "headers", authUsername: string | null, 
/**
 * HLS push credentials are write-only, independently of pull auth.
 */
pushAuthKind: "none" | "basic", pushAuthUsername: string | null, ingestUrls: IngestUrls | null, previewUrl: string, runtime: InputRuntimeState, };

export type InputRuntimeState = { connected: boolean, forwardingState: InputForwardState | null, protocol: string | null, remoteAddr: string | null, video: VideoDescriptor | null, audio: AudioDescriptor | null, };

export type VideoDescriptor = { codec: string, width: number, height: number, };

export type AudioDescriptor = { codec: string, sampleRate: number, channels: number, };

export type IngestUrls = { rtmp: string, srt: string, 
/**
 * HLS push (`-f hls -method PUT|POST`): the playlist URL; the segments
 * go next to it.
 */
hls: string, 
/**
 * MoQ publish (`https://`: WebTransport or QUIC, moq.dev convention): the
 * last path segment is the stream key. `None`
 * while the MoQ ingest listener is off.
 */
moq: string | null, };

export type PipelineInputRole = "primary" | "backup";

export type PipelineInputSourceKind = "keyed" | "hlsPull";

export type VariantRule = "highestBandwidth" | "lowestBandwidth";

export type InputForwardState = "standby" | "awaitingKeyframe" | "active";

export type PipelineGraph = { pipelineId: string, nodes: Array<GraphNode>, edges: Array<GraphEdge>, };

export type GraphNode = { id: string, 
/**
 * `ingest`, `demux`, `ring_buffer`, a stage's type, `packetizer`,
 * `egress`, `recirculation_input`, `recording` or `hls`.
 */
type: string, label: string, active: boolean, 
/**
 * On transcoder stage nodes: the stage (`<kind>` within the pipeline).
 */
stageKey: string | null, 
/**
 * On `egress` nodes: the output the node sends for.
 */
outputId: string | null, 
/**
 * The `stageKey` of the node's row in the engine telemetry's `stages`,
 * where its counters are; `null` for nodes counted elsewhere (ingest
 * and demux in `ingests`, the ring in `sourceRings`, egress in
 * `egresses`).
 */
metricsStage: string | null, };

export type GraphEdge = { from: string, to: string, label: string, };

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

export type EgressProtocol = "rtmp" | "srt" | "hls" | "sink" | "pipeline" | "moq" | "unknown";

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

export type FleetSample = { 
/**
 * Unix epoch milliseconds.
 */
atMs: number, 
/**
 * 100 less the capacity snapshot's projected utilization, floored at 0.
 */
headroomPercent: number, 
/**
 * Wanted outputs delivering, as a percentage; `null` when none is wanted.
 */
deliveringPercent: number | null, degraded: DegradedByLayer, };

export type DegradedByLayer = { ingest: number, feed: number, shard: number, destination: number, };

export type FleetHistoryWindow = { samples: Array<FleetSample>, completeFromMs: number, intervalMs: number, 
/**
 * The newest sample the server keeps, whatever the window: once it is
 * at or past a closed window's end, every sample in that window was
 * taken (server time, so a client's clock does not matter).
 */
newestMs: number | null, };
