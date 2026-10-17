import type { CapacitySnapshot } from "./core/generated/runtime-state.js";

export interface VideoTrack {
  codec?: string;
  width?: number;
  height?: number;
  fps?: number;
  pid?: number | null;
  language?: string | null;
  title?: string | null;
  profile?: string;
  level?: string;
  bw?: number | null;
}

export interface AudioTrack {
  index?: number | null;
  pid?: number | null;
  codec?: string;
  channels?: number;
  sample_rate?: number;
  language?: string | null;
  title?: string | null;
  profile?: string;
}

export interface VideoTrackSelection {
  mode: "firstVideoOnly";
  selectedTrackIndex?: number | null;
  availableTrackCount: number;
  ignoredTrackCount: number;
}

export interface IngestUrls {
  rtmp: string | null;
  srt: string | null;
}

export interface PipelineInputRuntime {
  connected: boolean;
  forwardingState: "standby" | "awaiting_keyframe" | "active" | null;
  protocol: string | null;
  uptimeSeconds: number | null;
  bytesReceived: number;
  remoteAddr: string | null;
  video: VideoTrack | null;
  audio: AudioTrack | null;
  quality: PublisherQuality | null;
}

/** How an input receives media: a publisher that presents its stream key on
 * one of the ingest listeners, or a remote HLS origin the engine pulls. */
export type PipelineInputKind = "keyed" | "hlsPull";

/** Which rendition of a remote master playlist a pull input follows. */
export type VariantRule = "highestBandwidth" | "lowestBandwidth";

/** How a pull input authenticates to its origin. The credential is write-only:
 * the API returns only this variant and, for "basic", a username. */
export type PullAuthKind = "none" | "bearer" | "basic" | "headers";

export interface PipelineInput {
  id: string;
  pipelineId: string;
  label: string;
  streamKey: string;
  role: "primary" | "backup";
  enabled: boolean;
  selected: boolean;
  /** "keyed" waits for a publisher on `ingestUrls`; "hlsPull" fetches
   * `sourceUrl` and accepts no pushes. */
  kind: PipelineInputKind;
  /** The remote origin, for a pull input only. */
  sourceUrl: string | null;
  variantRule: VariantRule;
  authKind: PullAuthKind;
  /** A basic username is not a secret; every other credential field is. */
  authUsername: string | null;
  /** null for a pull input: it accepts no pushes. */
  ingestUrls: IngestUrls | null;
  previewUrl: string;
  runtime: PipelineInputRuntime;
}

export interface PipelineInputsResponse {
  inputs: PipelineInput[];
  selectedInputId: string | null;
}

export interface PipelineInputMutationResponse {
  input: PipelineInput;
}

export interface PipelineInputPromotionResponse
  extends PipelineInputMutationResponse {
  connected: boolean;
}

export interface SrtGlobalIngestConfig {
  mode: "plaintext" | "encrypted";
  passphrase?: string | null;
  pbkeylen: 16 | 24 | 32;
  /** SRTO_RCVLATENCY (ms) for every ingest connection without a per-pipeline override. 20-8000. */
  latencyMs: number;
}

export interface SrtPipelineIngestConfig {
  mode: "inherit" | "plaintext" | "encrypted";
  passphrase?: string | null;
  pbkeylen?: 16 | 24 | 32 | null;
  /** `null`/absent inherits SrtGlobalIngestConfig.latencyMs. 20-8000. */
  latencyMs?: number | null;
}

export interface PublisherQuality {
  inboundRtpPacketsLost?: number;
  inboundRtpPacketsInError?: number;
  inboundRtpPacketsJitter?: number;
  msRtt?: number | null;
  mbpsReceiveRate?: number;
  packetsReceivedLoss?: number;
  packetsReceivedDrop?: number;
  packetsReceivedRetrans?: number;
  packetsReceivedUndecrypt?: number;
  packetsReceivedLossPerSec?: number | null;
  packetsReceivedDropPerSec?: number | null;
  packetsReceivedRetransPerSec?: number | null;
  packetsReceivedUndecryptPerSec?: number | null;
  msReceiveTsbPdDelay?: number | null;
  msReceiveBuf?: number | null;
  mbpsLinkCapacity?: number | null;
  packetsSentNAK?: number | null;
  srtBonded?: boolean | null;
  srtGroupMemberCount?: number | null;
  srtGroupConnectedMembers?: number | null;
  srtGroupActiveMembers?: number | null;
  srtGroupBrokenMembers?: number | null;
  srtGroupWireReceiverPacketsLost?: number | null;
  srtGroupWirePacketsUndecryptable?: number | null;
  srtRecvBufPackets?: number | null;
  srtRecvBufCapacityPackets?: number | null;
  srtRecvBufPayloadBytes?: number | null;
  tcpRttMs?: number | null;
  tcpRttVarMs?: number | null;
  tcpBytesReceived?: number | null;
  tcpLastRcvMs?: number | null;
  tcpRcvRttMs?: number | null;
  tcpRcvSpace?: number | null;
  tcpRcvOoopack?: number | null;
  tcpSkmemRmemAlloc?: number | null;
  tcpSkmemRmemMax?: number | null;
  tcpReceiveRateMbps?: number | null;
  tcpStatsUnavailableReason?: "not_linux" | "collection_failed" | string;
}

export interface Publisher {
  protocol: string;
  remoteAddr?: string;
  quality?: PublisherQuality;
}

export interface ConfigPipeline {
  id: string;
  name: string;
  streamKey: string;
  inputSource?: string | null;
  srtIngestPolicy?: SrtPipelineIngestConfig | null;
  ingestUrls?: IngestUrls;
  fileIngest?: PipelineFileIngestState | null;
}

export interface PipelineFileIngestState {
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

export interface ConfigOutput {
  id: string;
  pipelineId: string;
  name: string;
  url: string;
  monitoringUrl?: string | null;
  config: OutputConfig;
  desiredState?: string;
}

export type RtmpOutputMode = "legacy" | "enhanced";
export type OutputVideoCodec = "auto" | "h264" | "h265";

export type OutputProtocolConfig =
  | { type: "auto" }
  | { type: "rtmp"; mode: RtmpOutputMode };

export type OutputConfig = {
  video:
    | { mode: "source"; codec?: OutputVideoCodec }
    | { mode: "custom" }
    | { mode: "preset"; preset: string; codec?: OutputVideoCodec };
  audio:
    | { mode: "all" }
    | { mode: "selectTracks"; tracks: number[] }
    | {
        mode: "remap";
        track?: number;
        leftChannel: number;
        rightChannel: number;
      }
    | { mode: "downmix"; track: number };
  protocol?: OutputProtocolConfig;
};

export interface Job {
  pipelineId: string;
  outputId: string;
  startedAt?: string;
  endedAt?: string;
}

export interface ConfigData {
  serverName?: string;
  ingestHost?: string;
  dashboardPasswordChangeRecommended?: boolean;
  ingestSecurity?: IngestSecurityConfig;
  recordingSettings?: RecordingSettings;
  srtIngest?: SrtGlobalIngestConfig;
  transcodeProfiles?: Record<string, TranscodeProfileEntry>;
  pipelines: ConfigPipeline[];
  outputs: ConfigOutput[];
  jobs: Job[];
}

export interface RecordingSettings {
  retainSourceTs: boolean;
}

export interface TranscodeProfileEntry {
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

export interface IngestSecurityConfig {
  failureLimit: number;
  failureWindowMs: number;
  banMs: number;
  trackedIpLimit: number;
}

export interface InputHealth {
  status?: string;
  bytesReceived?: number;
  bytesSent?: number;
  readers?: number;
  bitrateKbps?: number | null;
  lastProgressAgeMs?: number | null;
  publishStartedAt?: string;
  probeReady?: boolean;
  probeStatus?: string;
  probePendingMs?: number | null;
  video?: VideoTrack;
  videoTrackSelection?: VideoTrackSelection | null;
  audio?: AudioTrack;
  audioTracks?: AudioTrack[];
  publisher?: Publisher;
  unexpectedReaders?: { count: number };
  lastSessionProtocol?: string | null;
  lastDisconnectAt?: string | null;
  lastDisconnectAgeMs?: number | null;
  lastDisconnectReason?: string | null;
  lastFailurePhase?: string | null;
  recentDisconnectError?: boolean;
  recentDisconnectCount?: number;
  flapping?: boolean;
  disconnectGraceActive?: boolean;
  disconnectGraceRemainingMs?: number | null;
  lastRemoteAddr?: string | null;
  lastSessionBytesReceived?: number | null;
}

export interface OutputHealth {
  status?: string;
  rawStatus?: string;
  phase?: string;
  uptimeSecs?: number | null;
  totalSize?: number | null;
  bitrateKbps?: number | null;
  lastProgressAt?: string | null;
  lastProgressAgeMs?: number | null;
  lastError?: string | null;
  lastErrorAt?: string | null;
  failurePhase?: string | null;
  recentFailureCount?: number;
  flapping?: boolean;
  retrying?: boolean;
  retryAttempts?: number | null;
  retryBackoffMs?: number | null;
  nextRetryAt?: string | null;
  retryRemainingMs?: number | null;
}

export interface HlsPreviewHealth {
  active?: boolean;
  persistentConsumers?: number;
  lastAccessAgeMs?: number | null;
  segments?: number;
  playlistBytes?: number;
}

export interface RuntimeNofileLimit {
  configured?: number | null;
  soft?: number | null;
  hard?: number | null;
  satisfied?: boolean;
  error?: string;
  unsupported?: boolean;
}

export interface HostSettingRow {
  key: string;
  label: string;
  current?: number | string | null;
  required?: number | string | null;
  unit?: string;
  status?: "ok" | "warning" | "unknown" | string;
  detail?: string | null;
}

export interface PipelineHealth {
  input?: InputHealth;
  outputs?: Record<string, OutputHealth>;
  recording?: { enabled: boolean; active: boolean };
  hlsPreview?: HlsPreviewHealth;
}

export interface HealthData {
  status?: string;
  pipelines?: Record<string, PipelineHealth>;
  runtimeLimits?: {
    nofile?: RuntimeNofileLimit;
  };
  hostSettings?: HostSettingRow[];
  rtmpListener?: {
    acceptErrors?: number;
    fdExhaustionErrors?: number;
    restarts?: number;
    /** Ingress owners accepting now; 0 while the listener is stopped. */
    ingressOwners?: number;
  };
  srtListener?: {
    bondingAvailable?: boolean;
    restarts?: number;
    /** Summed over every running SRT ingress Owner. */
    ingressOwner?: SrtIngressOwnerHealth;
  } | null;
  egressFabricShards?: EgressShardStatus[];
  containedPanics?: number;
}

/** One egress fabric shard as the engine reports it (`/engine/health`
 * `egressFabricShards`, `/metrics/system` `egressShards`). */
export interface EgressShardStatus {
  protocol: string;
  feedId: string;
  shardIndex: number;
  state: string;
  /** Commands queued for the shard thread, and the queue's bound. */
  commandDepth: number;
  commandCapacity: number;
  /** Since the shard last made progress; `null` before its first. */
  progressAgeMs: number | null;
}

/** The SRT ingress Owners' counters (cumulative) and gauges (now). */
export interface SrtIngressOwnerHealth {
  faulted: boolean;
  owners: number;
  peers: number;
  rxRingDepth: number;
  rxRingDropped: number;
  rxBufferExhaustions: number;
  txInFlight: number;
  txCapacity: number;
  budgetExhausted: number;
  eventBridgeFullVisits: number;
  telemetryDropped: number;
  overloadDisconnects: number;
  mediaSlowPasses5ms: number;
  mediaSlowPasses20ms: number;
  mediaPassMaxUs: number;
}

export interface SystemMetrics {
  generatedAt?: string;
  cpu?: {
    usagePercent?: number | null;
    cores?: number | null;
    load1?: number | null;
  };
  memory?: {
    usedBytes?: number | null;
    totalBytes?: number | null;
    usedPercent?: number | null;
  };
  engine?: {
    cpuPercent?: number | null;
    cpuSampleReady?: boolean;
    restreamCpuPercent?: number | null;
    externalFfmpegCpuPercent?: number | null;
    memoryBytes?: number | null;
    restreamMemoryBytes?: number | null;
    totalMemoryBytes?: number | null;
    externalFfmpegCount?: number | null;
    externalFfmpegMemoryBytes?: number | null;
  };
  disk?: {
    usedPercent?: number | null;
    totalBytes?: number | null;
    usedBytes?: number | null;
    freeBytes?: number | null;
    scope?: string;
    mountPoint?: string | null;
    root?: string;
  };
  capacity?: CapacitySnapshot;
  mediaDisk?: {
    usedPercent?: number | null;
    totalBytes?: number | null;
    usedBytes?: number | null;
    freeBytes?: number | null;
    scope?: string;
    mountPoint?: string | null;
    mediaDir?: string;
    mediaRoot?: string;
  };
  network?: {
    scope?: "external" | string;
    downloadKbps?: number | null;
    uploadKbps?: number | null;
    interfaces?: Array<{
      name: string;
      downloadKbps?: number | null;
      uploadKbps?: number | null;
    }>;
    ignoredInterfaces?: string[];
    sampleMs?: number | null;
  };
  /** MEDIA: the worker pool that runs per-packet media services; `workers`
   * lists the running ones (none until the first service starts). */
  mediaExecutor?: { configuredWorkers?: number; workers?: unknown[] };
  ioUring?: { available?: boolean; deploymentTier?: string };
  rtmps?: { ktlsCapabilities?: Record<string, boolean> };
  egressShards?: EgressShardStatus[];
}

export interface AppLogRow {
  id?: number;
  ts?: string;
  level?: string;
  target?: string;
  message?: string;
  fields?: string | Record<string, unknown> | null;
  pipelineId?: string | null;
  outputId?: string | null;
  eventType?: string | null;
  eventClass?: string | null;
}

export interface DiagnosticResult {
  index: number;
  name: string;
  description: string;
  command: string;
  stdout: string;
  stderr: string;
  exitCode: number;
  durationMs: number;
  help?: string;
  issues: string[];
}

export interface DiagnosticsReport {
  protocol: "rtmp" | "srt" | "file" | string;
  totalDurationMs: number;
  checks: DiagnosticResult[];
}
