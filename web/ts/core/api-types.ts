// API type definitions — extracted from api.ts to keep each file under 1,000 lines.
// Import types from here when only types are needed.
// Value imports (apiRequest, getConfig, …) still come from ./api.js.

import type {
  ConfigPipeline,
  ConfigOutput,
  OutputConfig,
  PipelineInputKind,
  PublisherQuality,
  SrtPipelineIngestConfig,
  VariantRule,
  AppLogRow,
} from "../types.js";
import type {
  CapacitySnapshot,
  Diagnosis,
  EgressProtocol,
  ServiceCenter,
} from "./generated/runtime-state.js";

export type AlertSeverity = "critical" | "warning";
export type AlertScope = "engine" | "pipeline" | "stage" | "output";

export interface OperatorAlert {
  id: string;
  severity: AlertSeverity;
  scope: AlertScope;
  pipelineId?: string;
  stageId?: string;
  outputId?: string;
  title: string;
  cause: string;
  evidence: AlertEvidence[];
  recommendedAction: string;
  generatedAt: string;
  firstSeen?: string;
  lastSeen?: string;
  /** Output alerts: the degraded layer, its cause and what was ruled out. */
  diagnosis?: Diagnosis;
}

/** One evidence line and how the engine knows it: `measured` is a reading
 * taken by the engine, `derived` a conclusion its rules drew from readings. */
export interface AlertEvidence {
  signal: string;
  confidence: "measured" | "derived";
}

/** An alert no longer observed: it ended between `alert.lastSeen` and
 * `resolvedAt`. */
export interface ResolvedAlert {
  alert: OperatorAlert;
  resolvedAt: string;
}

export interface AlertsSnapshot {
  generatedAt: string;
  alerts: OperatorAlert[];
  /** Only when asked with `since`: those resolved since, newest first. */
  resolved?: ResolvedAlert[];
  /** From when the engine's resolved history is complete. */
  resolvedCompleteFrom?: string;
}

export interface LifecycleEvent {
  seq: number;
  timestamp: string;
  kind: string;
  pipelineId: string;
  protocol?: string;
  encoding?: string;
  backend?: string;
  outputId?: string;
  phase?: string;
  error?: string;
}

export interface LifecycleEventsSnapshot {
  generatedAt: string;
  count: number;
  events: LifecycleEvent[];
}

/** How an added output sends, for packet pricing. */
export type OutputProtocol = "rtmp" | "rtmps" | "srt";

export type LimitSource = "prior" | "configured";

export interface CapacityLimits {
  ingressPps: number;
  egressPps: number;
  nicBps: number;
  memoryBytes: number;
  ffmpegStages: number;
  diskBps: number;
}

/** What a projection was priced on, and where each limit came from. */
export interface CapacityCalibration {
  modelVersion: number;
  limits: CapacityLimits;
  sources: Record<keyof CapacityLimits, LimitSource>;
  /** The CPU count the priors scale with. */
  priorCpus: number;
  safeUtilization: number;
}

/** A capacity scenario: one workload sample priced on the priors as it is
 * (`baseline`) and with the change (`projected`). Both are priors only;
 * neither carries a measured flow. Observe-only. */
export interface CapacityScenario {
  addOutputs: Record<OutputProtocol, number>;
  addStages: number;
  /** The pipeline whose ingest the added outputs carry; `null`: none live. */
  ratedBy: string | null;
  ingest: { bps: number; pps: number };
  calibration: CapacityCalibration;
  /** The engine's output reconnect policy: exponential from `baseMs`,
   * capped at `maxMs`, half fixed and half jittered ("equal" jitter). */
  retryPolicy: { maxRetries: number; baseMs: number; maxMs: number; jitter: "equal" };
  baseline: CapacitySnapshot;
  projected: CapacitySnapshot;
}

export type TelemetryMetrics = Record<string, number | string | boolean | null>;

export interface TelemetryIngest {
  pipelineId?: string;
  protocol: string;
  streamKey?: string;
  uptimeSecs: number;
  bytesReceived: number;
  video?: unknown;
  audio?: unknown;
  metrics: TelemetryMetrics;
}

/** One input session's counters; its state is the runtime state's input. */
export interface TelemetryInput {
  inputId: string;
  pipelineId: string;
  uptimeSecs: number;
  bytesReceived: number;
  quality: PublisherQuality;
}

export interface TelemetryRingReader {
  name: string;
  lagSlots?: number;
  overflowCount?: number;
  [key: string]: unknown;
}

/** A pipeline's source ring: how full it is and how far its readers lag. */
export interface TelemetrySourceRing {
  pipelineId: string;
  fill: number;
  capacity: number;
  payloadStats: TelemetryMetrics;
  readers: TelemetryRingReader[];
}

export interface TelemetryStage {
  stageKey?: string;
  pipelineId?: string;
  kind: string;
  active?: boolean;
  metrics: TelemetryMetrics;
  pipeMetrics?: TelemetryMetrics;
  lifecycle?: Record<string, unknown>;
  payloadStats?: TelemetryMetrics;
}

export interface TelemetryEgress {
  outputId: string;
  pipelineId?: string;
  protocol?: string;
  targetUrl?: string;
  targetAddr?: string | null;
  status?: string;
  phase?: string;
  uptimeSecs?: number;
  bytesOut?: number;
  lastProgressAt?: string | null;
  lastProgressAgeMs?: number | null;
  lastError?: string | null;
  failurePhase?: string | null;
  quality?: TelemetryMetrics;
  metrics?: TelemetryMetrics;
  /** Times the egress cursor resynchronised to its feed. */
  resyncCount?: number;
  /** How far the egress cursor is behind its feed. */
  feedLagUnits?: number;
  /** The fabric feed the output reads; with `shardId` it names the shard
   * in the engine health's `egressFabricShards`. */
  fabricFeed?: string | null;
  shardId?: number | null;
}

export interface EngineTelemetrySnapshot {
  generatedAt: string;
  ingests: TelemetryIngest[];
  inputs: TelemetryInput[];
  stages: TelemetryStage[];
  egresses: TelemetryEgress[];
  activeTranscoderBuffers: number;
  memoryAccounting?: { sourceRings?: TelemetrySourceRing[]; [key: string]: unknown };
}

export interface StageTelemetrySnapshot extends TelemetryStage {
  generatedAt: string;
  stageKey: string;
  pipelineId: string;
}

export interface ResourceMapMemory {
  attributedBytes?: number | null;
  confidence?: "measured" | "derived" | "estimated" | string;
  source?: string;
}

export interface ResourceMapNode {
  id: string;
  kind: string;
  label: string;
  pipelineId?: string | null;
  execution?: string;
  cpuPercent?: number | null;
  memory?: ResourceMapMemory | null;
  threads?: Record<string, number | string | null>;
  status?: string | null;
  phase?: string | null;
  metrics?: TelemetryMetrics;
  hotspots?: string[];
  /** `egress_shard` nodes: `healthy`, `stalled`, `panicked` or `stopped`. */
  state?: string;
  protocol?: string;
  shardIndex?: number;
  feedId?: string;
  /** A stage's FFmpeg child, when it has one. */
  backendPid?: number;
  /** Whether the node limits the pipeline, from the capacity model; absent
   * for a node that consumes no modelled service center. */
  judgement?: NodeJudgement;
}

export interface NodeJudgement {
  verdict: "limiting" | "pressured" | "clear";
  center: ServiceCenter;
  /** The shard's command queue against its bound, or the shared pool's
   * utilization against its calibrated limit. */
  utilization: number;
  /** A shard's own measurement, not the calibration priors. */
  measured: boolean;
  reason: string;
}

export interface ResourceMapSnapshot {
  generatedAt: string;
  scope: {
    kind: "runtime" | "pipeline" | string;
    pipelineId?: string | null;
  };
  view?: "summary" | "grouped" | "detail" | string;
  limits?: {
    topN?: number;
    totalNodeCount?: number;
    returnedNodeCount?: number;
    truncatedNodeCount?: number;
    maxTopN?: number;
  };
  summary: Record<string, number | string | boolean | null>;
  memoryAccounting?: Record<string, unknown> | null;
  nodes: ResourceMapNode[];
  edges?: Array<Record<string, unknown>>;
  attribution?: Record<string, string[]>;
}

export interface PipelineSummarySnapshot {
  generatedAt: string;
  pipelineId: string;
  input?: Record<string, unknown>;
  source?: {
    status?: string;
    bitrateKbps?: number | null;
    protocol?: string | null;
    readers?: number | null;
  };
  outputs?: {
    total?: number;
    running?: number;
    list?: Array<{
      id: string;
      status?: string;
      bitrateKbps?: number | null;
    }>;
  };
  /** `enabled`: recording is wanted (armed); `active`: a file is being written. */
  recording?: { enabled: boolean; active: boolean } | null;
  hlsPreview?: Record<string, unknown>;
  graph?: {
    nodes?: number;
    edges?: number;
    activeNodes?: number;
    inactiveNodes?: number;
    hasGraph?: boolean;
  };
  alerts?: OperatorAlert[];
}

export interface BuildLogsStreamUrlOptions {
  level?: string | null;
  target?: string | null;
  scope?: string | null;
  pipelineId?: string | null;
  outputId?: string | null;
  eventClass?: string | null;
  includeRestream?: boolean;
  lastEventId?: number | null;
  prefixes?: string[] | null;
}

export interface PipelineMutationResponse {
  message?: string;
  pipeline: ConfigPipeline;
}

export interface OutputMutationResponse {
  message?: string;
  desiredState?: string;
  output: ConfigOutput;
}

export interface OutputMutationArgs {
  name: string;
  url: string;
  monitoringUrl?: string | null;
  config: OutputConfig;
}

export interface TranscodeProfile {
  preset: string;
  tune: string;
  crf: number;
  gop: number;
  bframes: number;
  bitrate: number;
  maxBitrate: number;
  width: number;
  height: number;
}

export type TranscodeProfiles = Record<string, TranscodeProfile>;

export interface MediaFile {
  name: string;
  size: number;
  modifiedAt: string;
  ingestCount?: number;
  kind?: "recording" | "source";
  sourceName?: string;
  sourceSize?: number;
  convertedName?: string | null;
  convertedSize?: number | null;
  playName?: string | null;
  conversionStatus?: "converting" | "ready" | "failed" | null;
  conversionError?: string | null;
  conversionUpdatedAt?: string | null;
  /** The persisted recording session behind the file, when there is one
   * (`recordings` table, `MediaLibraryFile` in media_library_service.rs). */
  recordingId?: string | null;
  pipelineId?: string | null;
  recordingStatus?: "recording" | "finalizing" | "ready" | "failed" | null;
  recordingStartedAt?: string | null;
  recordingEndedAt?: string | null;
  recordingCodecSummary?: string | null;
  recordingError?: string | null;
}

export interface IngestConfig {
  id: string;
  filename: string;
  streamKey: string;
  loop: boolean;
  startTime: string;
  liveOptimized: boolean;
  targetGopSeconds: number;
  running: boolean;
}

export interface PipelineFileIngestConfig {
  configured: boolean;
  id?: string;
  filename?: string;
  streamKey?: string;
  loop?: boolean;
  startTime?: string;
  liveOptimized?: boolean;
  targetGopSeconds?: number;
  running: boolean;
}

export interface MediaFileAnalysis {
  videoCodec?: string | null;
  fps?: number | null;
  durationSec?: number | null;
  keyframeCount: number;
  averageKeyframeIntervalSec?: number | null;
  maxKeyframeIntervalSec?: number | null;
  sparseForLive: boolean;
  liveGopTargetSeconds: number;
}

export interface AudioCapsPayload {
  caps?: Record<
    string,
    {
      maxTracks?: number | null;
      maxChannels?: number | null;
      codecs?: string[] | "any" | null;
    }
  >;
  platformLabels?: Record<string, string>;
}

export interface RateLimitAttempt {
  scope: string;
  ip: string;
  failureCount: number;
  banned: boolean;
  banRemainingMs?: number | null;
}

export interface RateLimitState {
  attempts: RateLimitAttempt[];
}

/** `GET /api/v1/monitoring/youtube-status`: the watch page's live flags. */
export interface YoutubeMonitoringStatus {
  canonical_watch_url: string;
  live_now: boolean;
  live_content: boolean;
  upcoming: boolean;
  title: string | null;
}

/** One node of a pipeline's processing graph: the runtime state's node
 * (`PipelineGraph`) with the telemetry sample's counters attached
 * (`pipelineGraph` in `control-room/state-views.ts`), or one node of
 * `GET /api/v1/pipelines/{id}/graph`. */
export interface GraphNode {
  id: string;
  type: string;
  label: string;
  active: boolean;
  stageKey?: string;
  /** On `egress` nodes: the output the node sends for. */
  outputId?: string;
  details?: Record<string, unknown> | null;
  /** The node's counters (`packetsPerSec`, `avgUsPerPacket`, ...). */
  metrics?: TelemetryMetrics | null;
}

export interface GraphEdge {
  from: string;
  to: string;
  label: string;
}

export interface GraphData {
  pipelineId: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

/** One output change the engine's planner understands (`ProposedChange`). */
export interface ProposedChange {
  kind: "addOutput" | "updateOutput" | "removeOutput" | "startOutput" | "stopOutput";
  outputId?: string;
  name?: string;
  url?: string;
  config?: OutputConfig;
  desiredState?: "running" | "stopped";
}

export interface ChangeRequest {
  intent: string;
  pipelineId: string;
  proposedChanges: ProposedChange[];
}

export interface PlanIssue {
  severity: string;
  code: string;
  message: string;
  field?: string;
}

/** Wanted-running outputs that read one stage over one protocol. */
export interface TopologyRow {
  stage: string;
  protocol: EgressProtocol;
  outputs: number;
}

/** The plan pipeline's wanted-running outputs before and after a change,
 * planned for the live ingest's codec (`null`: no live ingest). */
export interface TopologyDiff {
  pipelineId: string;
  ingestCodec: string | null;
  before: TopologyRow[];
  after: TopologyRow[];
  addedStages: string[];
  retiredStages: string[];
}

/** `POST /agent/plans`: the engine's judgement of a change, made without
 * mutating anything. */
export interface ChangePlan {
  planId: string;
  executionEnabled: boolean;
  validation: { valid: boolean; errors: PlanIssue[]; warnings: PlanIssue[] };
  graphPreview: { notes: string[]; topology: TopologyDiff | null };
  impact: {
    affectedPipelines: string[];
    affectedOutputs: string[];
    sharedStageCandidates: string[];
    operatorSummary: string;
    engineeringNotes: string[];
  };
}

export type OperationStatus =
  | "invalid"
  | "awaitingApproval"
  | "approved"
  | "applying"
  | "applied"
  | "verified"
  | "verificationFailed"
  | "failed";

/** A fixed verification gate; all four must pass for `verified`. */
export interface VerifyGate {
  gate: "desiredState" | "outputsRunning" | "newCriticalAlerts" | "capacityState";
  passed: boolean;
  detail: string;
}

/** One change the apply made, as the engine recorded it. */
export interface StateTransition {
  at: string;
  kind: ProposedChange["kind"];
  pipelineId: string;
  outputId?: string | null;
  /** The output record before and after; `null` when there was none. */
  from?: { desiredState?: string } | null;
  to?: { desiredState?: string } | null;
}

export interface OperationCheck {
  kind: string;
  outputId?: string | null;
  passed: boolean;
  reason?: string | null;
}

/** An approval-gated operation (`public_record`): redacted, in memory only. */
export interface ChangeOperation {
  operationId: string;
  status: OperationStatus;
  intentSummary: string;
  /** The request it was created from; secrets such as URLs are redacted. */
  request: ChangeRequest;
  approval: { approvedBy: string; claimedBy: string; reason: string | null; approvedAt: string } | null;
  auditLog: { at: string; kind: string; summary: string }[];
  stateTransitions: StateTransition[];
  /** A failed apply stops at the first rejected change; earlier ones stay applied. */
  executionResult:
    | {
        success: true;
        changeResults: { kind: string; outputId?: string | null; status: string }[];
      }
    | { success: false; error: string }
    | null;
  /** `gates` decide `verified`; the alert delta and graph are reported. */
  verificationResult: {
    explanation?: string;
    gates?: VerifyGate[];
    checks?: OperationCheck[];
    incidentDelta?: { delta: number };
    graphConvergence?: { activeNodes?: number };
  } | null;
}
/** The body of a pipeline create or update. On update an omitted field keeps
 * its stored value, `null` clears it, and an empty `streamKey` (as on create)
 * asks for a new key. */
export interface PipelineWrite {
  name: string;
  streamKey?: string;
  inputSource?: string | null;
  srtIngestPolicy?: SrtPipelineIngestConfig | null;
  fileIngest?: {
    filename: string;
    loopFlag: boolean;
    startTime: string;
    liveOptimized: boolean;
    targetGopSeconds: number;
  } | null;
}

/** One request header a pull input presents to its origin. Its value is a
 * credential: stored and used, never returned. */
export interface PullHeaderPair {
  name: string;
  value: string;
}

/** The `auth` object of a pipeline input write: one tagged mechanism. A signed
 * URL carries its own token and the fetcher's cookie jar carries what the
 * origin sets, so `None` covers both. Every secret here is write-only. */
export type PullAuthPayload =
  | { kind: "none" }
  | { kind: "bearer"; token: string }
  /** The whole cookie string of a signed cookie, sent as `Cookie`. */
  | { kind: "cookies"; cookies: string }
  | { kind: "basic"; username: string; password: string }
  | { kind: "headers"; headers: PullHeaderPair[] };

/** HLS publisher credentials, write-only; omission preserves stored auth. */
export type PushAuthPayload =
  | { kind: "none" }
  | { kind: "basic"; username: string; password: string };

/** The body of a pipeline input create. */
export interface PipelineInputWrite {
  label: string;
  kind: PipelineInputKind;
  /** The remote origin; null when `kind` is "keyed", which accepts pushes. */
  sourceUrl?: string | null;
  variantRule?: VariantRule;
  auth?: PullAuthPayload;
  pushAuth?: PushAuthPayload;
}

/** The body of a pipeline input update. An omitted `auth` keeps the stored
 * credential; `{"kind":"none"}` clears it. */
export interface PipelineInputPatch extends Partial<PipelineInputWrite> {
  enabled?: boolean;
}

export interface ProbeAudioTrack {
  trackIndex: number;
  codec?: string | null;
  channels?: number | null;
  channelLayout?: string | null;
  sampleRate?: number | null;
}

/** `GET /pipelines/{id}/probe`: what the live publisher sends (404 without one). */
export interface PipelineProbe {
  audioTracks: ProbeAudioTrack[];
}

/** `POST /pipelines/{id}/moq-token`: a viewer grant for the pipeline's MoQ
 * broadcast; the token starts a session within 60 s. */
export interface MoqPreviewGrant {
  token: string;
  /** `https://host:port/?token=…`: WebTransport straight to the engine. */
  url: string;
  /** The broadcast name (the pipeline id). */
  broadcast: string;
  expiresAt: string;
  /** Hex SHA-256 of the listener's leaf certificate: pinned when normal
   * certificate checks fail (a short-lived self-signed certificate). */
  certificateSha256: string | null;
}

/** Filters shared by `GET /logs` and `GET /logs/stream`. `level` is the
 * lowest level shown; `scope` "restream" means engine-wide rows only. */
export interface LogFilters {
  level?: "error" | "warn" | "info" | "debug";
  scope?: "restream" | "pipeline" | "output";
  pipelineId?: string;
  outputId?: string;
  eventClass?: "lifecycle";
}

export interface LogPage {
  logs: AppLogRow[];
  hasMore: boolean;
}

/** The selected ingest at one recorded instant; a value the publisher's
 * protocol does not report is absent (`IngestHealth` in
 * `application/flight_recorder.rs`). */
export interface IngestHealth {
  inputId: string;
  protocol: string;
  bitrateBps: number | null;
  packetsPerSec: number;
  videoCodec: string | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  /** Mean and longest gap between the ingest's last keyframes. */
  gopSec: number | null;
  maxGopSec: number | null;
  rttMs: number | null;
  lossPerSec: number | null;
  retransPerSec: number | null;
  /** SRT receive buffer fill, 0..1. */
  receiveBuffer: number | null;
}

export interface RingHealth {
  fill: number;
  capacity: number;
  worstLagSlots: number;
  overflows: number;
}

export interface OutputsHealth {
  wanted: number;
  delivering: number;
  degraded: number;
  retrying: number;
  stalled: number;
  retryAttempts: number;
  offeredBps: number;
  deliveredBps: number;
  resyncs: number;
  worstFeedLag: number;
}

export interface FlightSample {
  atMs: number;
  ingest: IngestHealth | null;
  ring: RingHealth | null;
  outputs: OutputsHealth;
}

/** `GET /pipelines/{id}/flight-recorder`: a pipeline's recorded health,
 * oldest first, sampled every `intervalMs` (`restream.trace.v1`). */
export interface FlightTrace {
  schema: "restream.trace.v1";
  pipelineId: string;
  intervalMs: number;
  completeFromMs: number;
  samples: FlightSample[];
}
