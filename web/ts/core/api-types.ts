// API type definitions — extracted from api.ts to keep each file under 1,000 lines.
// Import types from here when only types are needed.
// Value imports (apiRequest, getConfig, …) still come from ./api.js.

import type { ConfigPipeline, ConfigOutput, OutputConfig } from "../types.js";

export interface OverviewSnapshot {
  generatedAt: string;
  totalPipelines: number;
  activePipelines: number;
  degradedPipelines: number;
  failedOutputs: number;
  alertCount: { critical: number; warning: number };
  srtListener: Record<string, unknown> | null;
}

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
  evidence: string[];
  recommendedAction: string;
  generatedAt: string;
  firstSeen?: string;
  lastSeen?: string;
}

export interface AlertsSnapshot {
  generatedAt: string;
  alerts: OperatorAlert[];
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
}

export interface SourceRingReader {
  name: string;
  lagSlots: number;
  overflowCount: number;
  packetAgeMs: number | null;
}

export interface SourceRingTelemetry {
  fill: number;
  capacity: number;
  fillPercent: number;
  estimatedPktRatePerSec: number;
  bufferDepthSecs: number;
  payloadStats: TelemetryMetrics;
  readers: SourceRingReader[];
}

export interface EngineTelemetrySnapshot {
  generatedAt: string;
  ingests: TelemetryIngest[];
  stages: TelemetryStage[];
  egresses: TelemetryEgress[];
  activeTranscoderBuffers: number;
  memoryAccounting?: Record<string, unknown>;
}

export interface PipelineTelemetrySnapshot {
  generatedAt: string;
  pipelineId: string;
  ingest: TelemetryIngest | null;
  sourceRing: SourceRingTelemetry | null;
  stages: TelemetryStage[];
  egresses: TelemetryEgress[];
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
  queue?: TelemetryMetrics | null;
  hotspots?: string[];
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
  memoryAccounting?: Record<string, unknown>;
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
  recording?: Record<string, unknown>;
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

export interface YoutubeMonitoringStatus {
  canonical_watch_url: string;
  live_now: boolean;
  live_content: boolean;
  upcoming: boolean;
  title: string | null;
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
