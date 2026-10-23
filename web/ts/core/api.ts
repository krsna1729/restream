import { showLoading, hideLoading, showErrorAlert } from "./utils.js";
import { withBasePath } from "./base-path.js";
import { redirectToLogin } from "./auth-redirect.js";
import type {
  ConfigData,
  IngestSecurityConfig,
  RecordingSettings,
  SrtGlobalIngestConfig,
  DiagnosticsReport,
  PipelineInputMutationResponse,
  PipelineInputPromotionResponse,
  PipelineInputsResponse,
  SystemMetrics,
} from "../types.js";
import type {
  AlertsSnapshot,
  LifecycleEventsSnapshot,
  EngineTelemetrySnapshot,
  StageTelemetrySnapshot,
  ResourceMapSnapshot,
  CapacityScenario,
  OutputProtocol,
  PipelineSummarySnapshot,
  FlightTrace,
  Schedule,
  NotifyChannel,
  BuildLogsStreamUrlOptions,
  PipelineMutationResponse,
  OutputMutationResponse,
  OutputMutationArgs,
  MediaFile,
  IngestConfig,
  PipelineFileIngestConfig,
  MediaFileAnalysis,
  TranscodeProfiles,
  RateLimitState,
  YoutubeMonitoringStatus,
  ChangeRequest,
  ChangePlan,
  ChangeOperation,
  PipelineWrite,
  PipelineInputPatch,
  PipelineInputWrite,
  MoqPreviewGrant,
  PipelineProbe,
  LogFilters,
  LogPage,
} from "./api-types.js";
import type { FleetHistoryWindow } from "./generated/runtime-state.js";

let activeMutationRequestCount = 0;
const DEFAULT_ENGINE_SBOM_ENDPOINT = "/api/v1/engine/sbom";

function isMutationMethod(method: string): boolean {
  const normalizedMethod = String(method || "GET").toUpperCase();
  return (
    normalizedMethod !== "GET" &&
    normalizedMethod !== "HEAD" &&
    normalizedMethod !== "OPTIONS"
  );
}

function beginMutationRequest(): void {
  activeMutationRequestCount += 1;
  if (activeMutationRequestCount === 1) {
    showLoading();
  }
}

function endMutationRequest(): void {
  if (activeMutationRequestCount <= 0) {
    activeMutationRequestCount = 0;
    return;
  }

  activeMutationRequestCount -= 1;
  if (activeMutationRequestCount === 0) {
    hideLoading();
  }
}

interface ApiRequestOptions {
  method?: string;
  body?: unknown;
  signal?: AbortSignal;
  showMutationLoading?: boolean;
  silentStatuses?: number[];
}

async function apiRequest<T = unknown>(
  url: string,
  {
    method = "GET",
    body = null,
    signal,
    showMutationLoading = true,
    silentStatuses = [],
  }: ApiRequestOptions = {},
): Promise<T | null> {
  const normalizedMethod = String(method || "GET").toUpperCase();
  const options: RequestInit = { method: normalizedMethod, signal };

  if (body !== null) {
    if (body instanceof FormData) {
      options.body = body;
    } else {
      options.headers = { "Content-Type": "application/json" };
      options.body = JSON.stringify(body);
    }
  }

  const trackMutationLoading =
    showMutationLoading && isMutationMethod(normalizedMethod);
  let response: Response | null = null;
  if (trackMutationLoading) beginMutationRequest();
  try {
    response = await fetch(withBasePath(url), options);
  } catch (e) {
    if (signal?.aborted) return null;
    showErrorAlert("Network request failed: " + e);
    return null;
  } finally {
    if (trackMutationLoading) endMutationRequest();
  }

  if (response.status === 204) {
    return null;
  }

  if (response.status === 401) {
    redirectToLogin();
    return null;
  }

  if (silentStatuses.includes(response.status)) {
    return null;
  }

  let data: T | null = null;
  try {
    data = (await response.json()) as T;
  } catch (e) {
    showErrorAlert("Invalid JSON response: " + e);
    return null;
  }

  if (!response.ok) {
    const errData = data as Record<string, unknown> | null;
    showErrorAlert(errData?.error || `Request failed with ${response.status}`);
    return null;
  }

  return data;
}

interface GetConfigOptions {
  jobs?: "all" | "latest";
  view?: "full" | "dashboard";
}

async function getConfig(
  options: GetConfigOptions = {},
): Promise<ConfigData | null> {
  const query = new URLSearchParams();
  if (options.jobs === "latest") query.set("jobs", "latest");
  if (options.view === "dashboard") query.set("view", "dashboard");
  const suffix = query.toString();
  const url = suffix ? `/api/v1/settings?${suffix}` : "/api/v1/settings";
  // A viewer may not read settings (they hold stream keys): no config, no
  // alert; the views that use it work without.
  return apiRequest<ConfigData>(url, { silentStatuses: [403] });
}

interface GetSystemMetricsOptions {
  view?: "full" | "summary";
}

async function getSystemMetrics(
  options: GetSystemMetricsOptions = {},
): Promise<SystemMetrics | null> {
  const query = new URLSearchParams();
  if (options.view === "summary") query.set("view", "summary");
  const suffix = query.toString();
  const url = suffix ? `/metrics/system?${suffix}` : "/metrics/system";
  return apiRequest<SystemMetrics>(url);
}

async function getAggregateAlerts(
  options: { since?: Date } = {},
): Promise<AlertsSnapshot | null> {
  return apiRequest<AlertsSnapshot>(
    options.since
      ? `/api/v1/alerts?since=${encodeURIComponent(options.since.toISOString())}`
      : "/api/v1/alerts",
  );
}

/** The fleet's sampled signals (Q-064) taken from `since` through `until`
 * (each optional: every kept one, up to now), oldest first. */
async function getFleetHistory(
  options: { since?: Date; until?: Date } = {},
): Promise<FleetHistoryWindow | null> {
  const query = new URLSearchParams();
  if (options.since) query.set("since", options.since.toISOString());
  if (options.until) query.set("until", options.until.toISOString());
  const search = query.toString();
  return apiRequest<FleetHistoryWindow>(
    search ? `/api/v1/fleet/history?${search}` : "/api/v1/fleet/history",
  );
}

async function getLifecycleEvents(
  options: {
    pipelineId?: string | null;
    limit?: number;
  } = {},
): Promise<LifecycleEventsSnapshot | null> {
  const query = new URLSearchParams();
  if (options.pipelineId) query.set("pipeline_id", options.pipelineId);
  if (options.limit !== undefined) query.set("limit", String(options.limit));
  const suffix = query.toString();
  return apiRequest<LifecycleEventsSnapshot>(
    suffix ? `/api/v1/events?${suffix}` : "/api/v1/events",
  );
}

async function getEngineTelemetry(): Promise<EngineTelemetrySnapshot | null> {
  return apiRequest<EngineTelemetrySnapshot>("/api/v1/engine/telemetry");
}

async function getStageTelemetry(
  stageKey: string,
): Promise<StageTelemetrySnapshot | null> {
  return apiRequest<StageTelemetrySnapshot>(
    `/api/v1/stages/${encodeURIComponent(stageKey)}/telemetry`,
    { silentStatuses: [404] },
  );
}

async function getResourceMap(
  pipelineId?: string | null,
  options: { view?: "summary" | "grouped" | "detail"; topN?: number } = {},
): Promise<ResourceMapSnapshot | null> {
  const query = new URLSearchParams();
  if (pipelineId) query.set("pipeline_id", pipelineId);
  if (options.view) query.set("view", options.view);
  if (Number.isFinite(options.topN)) query.set("top_n", String(options.topN));
  const suffix = query.toString();
  return apiRequest<ResourceMapSnapshot>(
    suffix
      ? `/api/v1/engine/resource-map?${suffix}`
      : "/api/v1/engine/resource-map",
  );
}

async function getCapacityScenario(change: {
  addOutputs: Partial<Record<OutputProtocol, number>>;
  addStages: number;
  /** Whose ingest the outputs carry; the highest-bitrate live one when absent. */
  pipelineId?: string;
  /** The caller handles a pipeline with no live ingest (422) itself. */
  quietWhenNotLive?: boolean;
}): Promise<CapacityScenario | null> {
  const query = new URLSearchParams({ add_stages: String(change.addStages) });
  for (const [protocol, count] of Object.entries(change.addOutputs)) {
    if (count) query.set(`add_${protocol}`, String(count));
  }
  if (change.pipelineId) query.set("pipeline_id", change.pipelineId);
  return apiRequest<CapacityScenario>(
    `/api/v1/capacity/scenario?${query}`,
    change.quietWhenNotLive ? { silentStatuses: [422] } : {},
  );
}

async function getPipelineSummary(
  pipelineId: string,
): Promise<PipelineSummarySnapshot | null> {
  return apiRequest<PipelineSummarySnapshot>(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/summary`,
  );
}

/** The pipeline's recorded ingest, ring and output health
 * (`application/flight_recorder.rs`); null (quietly) before the recorder
 * has sampled it. */
async function getFlightRecorder(
  pipelineId: string,
  since?: Date,
  window: { until?: Date; points?: number } = {},
): Promise<FlightTrace | null> {
  const query = new URLSearchParams();
  if (since) query.set("since", since.toISOString());
  if (window.until) query.set("until", window.until.toISOString());
  if (window.points) query.set("points", String(window.points));
  const suffix = query.size > 0 ? `?${query}` : "";
  return apiRequest<FlightTrace>(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/flight-recorder${suffix}`,
    { silentStatuses: [404] },
  );
}

/** What the live publisher sends; null (quietly) when nobody publishes. */
async function getPipelineProbe(pipelineId: string): Promise<PipelineProbe | null> {
  return apiRequest<PipelineProbe>(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/probe`,
    { silentStatuses: [404] },
  );
}

/** A short-lived viewer grant for the pipeline's MoQ broadcast (its
 * program): the WebTransport URL carries the token. Null, quietly, when the
 * engine's MoQ listener is off (409) or the pipeline is gone (404). */
async function mintMoqPreview(pipelineId: string): Promise<MoqPreviewGrant | null> {
  return apiRequest<MoqPreviewGrant>(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/moq-token`,
    { method: "POST", showMutationLoading: false, silentStatuses: [404, 409] },
  );
}

async function getEngineStatus<T = unknown>(): Promise<T | null> {
  return apiRequest<T>("/api/v1/engine");
}

async function getEngineHealth<T = unknown>(): Promise<T | null> {
  return apiRequest<T>("/api/v1/engine/health");
}

function getEngineSbomEndpoint(
  status: { sbom?: { endpoint?: string | null } } | null | undefined,
): string {
  return status?.sbom?.endpoint || DEFAULT_ENGINE_SBOM_ENDPOINT;
}

async function getEngineSbom(endpoint: string): Promise<unknown | null> {
  return apiRequest(endpoint);
}

async function getAudioCapsPayload(): Promise<Record<string, unknown> | null> {
  return apiRequest<Record<string, unknown>>("/api/v1/audio-caps");
}

async function runPipelineDiagnostics(
  pipelineId: string,
  signal?: AbortSignal,
): Promise<DiagnosticsReport | null> {
  return apiRequest<DiagnosticsReport>(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/diagnostics/run`,
    {
      method: "POST",
      signal,
      showMutationLoading: false,
    },
  );
}

function buildLogsStreamUrl(options: BuildLogsStreamUrlOptions = {}): string {
  const query = new URLSearchParams();
  if (options.level) query.set("level", String(options.level));
  if (options.target) query.set("target", String(options.target));
  if (options.scope) query.set("scope", String(options.scope));
  if (options.pipelineId) query.set("pipeline_id", String(options.pipelineId));
  if (options.includeRestream) query.set("include_restream", "true");
  if (options.outputId) query.set("output_id", String(options.outputId));
  if (options.eventClass) query.set("event_class", String(options.eventClass));
  if (Number.isFinite(options.lastEventId as number)) {
    query.set("last_event_id", String(options.lastEventId));
  }
  if (Array.isArray(options.prefixes) && options.prefixes.length > 0) {
    query.set("prefix", options.prefixes.join(","));
  }
  const suffix = query.toString();
  return suffix ? `/api/v1/logs/stream?${suffix}` : "/api/v1/logs/stream";
}

/**
 * Runtime state stream (Q-042); `since` (`<epoch>.<revision>`) resumes after
 * that revision of that server run.
 */
export function buildRuntimeStateStreamUrl(since?: string): string {
  const path = since
    ? `/api/v1/state/stream?since=${encodeURIComponent(since)}`
    : "/api/v1/state/stream";
  return withBasePath(path);
}

/** A PNG still of a stored media file (the engine picks a frame a few
 * seconds in); 404 when the file has no picture. */
export function mediaPosterUrl(filename: string): string {
  return withBasePath(`/api/v1/media/${encodeURIComponent(filename)}/poster`);
}

/** The HLS preview of what a pipeline sends (its program). */
export function programPreviewUrl(pipelineId: string): string {
  return withBasePath(`/hls/${encodeURIComponent(pipelineId)}/master.m3u8`);
}

async function createPipeline(
  args: PipelineWrite,
): Promise<PipelineMutationResponse | null> {
  if (!args.name) {
    showErrorAlert("Invalid pipeline name");
    return null;
  }

  return apiRequest<PipelineMutationResponse>("/api/v1/pipelines", {
    method: "POST",
    body: args,
  });
}

async function getPipelineInputs(
  pipeId: string,
): Promise<PipelineInputsResponse | null> {
  return apiRequest<PipelineInputsResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/inputs`,
  );
}

async function createPipelineInput(
  pipeId: string,
  body: PipelineInputWrite,
): Promise<PipelineInputMutationResponse | null> {
  return apiRequest<PipelineInputMutationResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/inputs`,
    { method: "POST", body },
  );
}

/** An omitted `auth` keeps the stored credential: callers that do not change
 * the credential must not send one. */
async function updatePipelineInput(
  pipeId: string,
  inputId: string,
  data: PipelineInputPatch,
): Promise<PipelineInputMutationResponse | null> {
  return apiRequest<PipelineInputMutationResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/inputs/${encodeURIComponent(inputId)}`,
    { method: "PATCH", body: data },
  );
}

async function deletePipelineInput(
  pipeId: string,
  inputId: string,
): Promise<{ deleted: boolean } | null> {
  return apiRequest<{ deleted: boolean }>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/inputs/${encodeURIComponent(inputId)}`,
    { method: "DELETE" },
  );
}

async function promotePipelineInput(
  pipeId: string,
  inputId: string,
): Promise<PipelineInputPromotionResponse | null> {
  return apiRequest<PipelineInputPromotionResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/inputs/${encodeURIComponent(inputId)}/promote`,
    { method: "POST" },
  );
}

async function updatePipeline(
  pipeId: string,
  data: PipelineWrite,
): Promise<PipelineMutationResponse | null> {
  if (!pipeId) {
    showErrorAlert("Pipeline id is required");
    return null;
  }

  return apiRequest<PipelineMutationResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}`,
    {
      method: "PATCH",
      body: data,
    },
  );
}

async function deletePipeline(pipeId: string): Promise<unknown | null> {
  if (!pipeId) {
    showErrorAlert("Pipeline id is required");
    return null;
  }

  return apiRequest(`/api/v1/pipelines/${encodeURIComponent(pipeId)}`, {
    method: "DELETE",
  });
}

async function createOutput(
  pipeId: string,
  data: OutputMutationArgs,
): Promise<OutputMutationResponse | null> {
  if (!pipeId) {
    showErrorAlert("Pipeline id is required");
    return null;
  }

  return apiRequest<OutputMutationResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/outputs`,
    {
      method: "POST",
      body: data,
    },
  );
}

async function updateOutput(
  pipeId: string,
  outId: string,
  data: OutputMutationArgs,
): Promise<OutputMutationResponse | null> {
  if (!pipeId || !outId) {
    showErrorAlert("Pipeline id and output id are required");
    return null;
  }

  return apiRequest<OutputMutationResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/outputs/${encodeURIComponent(outId)}`,
    { method: "PATCH", body: data },
  );
}

async function deleteOutput(
  pipeId: string,
  outId: string,
): Promise<unknown | null> {
  if (!pipeId || !outId) {
    showErrorAlert("Pipeline id and output id are required");
    return null;
  }

  return apiRequest(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/outputs/${encodeURIComponent(outId)}`,
    { method: "DELETE" },
  );
}

async function startOut(
  pipeId: string,
  outId: string,
): Promise<OutputMutationResponse | null> {
  if (!pipeId || !outId) {
    showErrorAlert("Pipeline id and output id are required");
    return null;
  }

  return apiRequest<OutputMutationResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/outputs/${encodeURIComponent(outId)}/start`,
    { method: "POST" },
  );
}

async function stopOut(
  pipeId: string,
  outId: string,
): Promise<OutputMutationResponse | null> {
  if (!pipeId || !outId) {
    showErrorAlert("Pipeline id and output id are required");
    return null;
  }

  return apiRequest<OutputMutationResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/outputs/${encodeURIComponent(outId)}/stop`,
    { method: "POST" },
  );
}

/** Persisted log rows, newest first; `hasMore` when older rows exist. */
async function getLogs(filters: LogFilters, limit: number): Promise<LogPage | null> {
  const query = new URLSearchParams({ limit: String(limit) });
  if (filters.level) query.set("level", filters.level);
  if (filters.scope) query.set("scope", filters.scope);
  if (filters.pipelineId) query.set("pipeline_id", filters.pipelineId);
  if (filters.outputId) query.set("output_id", filters.outputId);
  if (filters.eventClass) query.set("event_class", filters.eventClass);
  return apiRequest<LogPage>(`/api/v1/logs?${query.toString()}`);
}

/** The settings a `PATCH /settings` may change; absent ones stay. */
export interface ConfigPatch {
  serverName?: string;
  ingestHost?: string;
  ingestSecurity?: Partial<IngestSecurityConfig>;
  recordingSettings?: RecordingSettings;
  srtIngest?: SrtGlobalIngestConfig;
  transcodeProfiles?: TranscodeProfiles;
}

/** The settings the engine stored after a patch. */
export interface StoredConfig {
  serverName: string;
  ingestHost: string;
  dashboardPasswordChangeRecommended?: boolean;
  ingestSecurity: IngestSecurityConfig;
  recordingSettings: RecordingSettings;
  srtIngest: SrtGlobalIngestConfig;
  transcodeProfiles?: TranscodeProfiles;
}

async function patchConfig(body: ConfigPatch): Promise<StoredConfig | null> {
  return apiRequest<StoredConfig>("/api/v1/settings", { method: "PATCH", body });
}

async function startRecording(
  pipeId: string,
): Promise<{ enabled: boolean; active: boolean } | null> {
  return apiRequest<{ enabled: boolean; active: boolean }>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/recording/start`,
    { method: "POST" },
  );
}

async function stopRecording(
  pipeId: string,
): Promise<{ enabled: boolean; active: boolean } | null> {
  return apiRequest<{ enabled: boolean; active: boolean }>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/recording/stop`,
    { method: "POST" },
  );
}

async function listMediaFiles(): Promise<{ files: MediaFile[] } | null> {
  return apiRequest<{ files: MediaFile[] }>("/api/v1/media");
}

/** Where the browser downloads or plays a media file. */
function mediaFileUrl(filename: string): string {
  return withBasePath(`/media/${encodeURIComponent(filename)}`);
}

async function uploadMediaFile(
  file: File,
): Promise<{ uploaded: boolean; name: string; size: number } | null> {
  const body = new FormData();
  body.append("file", file, file.name);
  return apiRequest<{ uploaded: boolean; name: string; size: number }>(
    "/api/v1/media/upload",
    { method: "POST", body },
  );
}

async function deleteMediaFile(
  filename: string,
): Promise<{ deleted: boolean } | null> {
  return apiRequest<{ deleted: boolean }>(
    `/api/v1/media/${encodeURIComponent(filename)}`,
    {
      method: "DELETE",
    },
  );
}

async function renameMediaFile(
  filename: string,
  newName: string,
): Promise<{ renamed: boolean; name: string; updatedIngests?: number } | null> {
  return apiRequest<{
    renamed: boolean;
    name: string;
    updatedIngests?: number;
  }>(`/api/v1/media/${encodeURIComponent(filename)}`, {
    method: "PATCH",
    body: { newName },
  });
}

async function startIngest(id: string): Promise<IngestConfig | null> {
  return apiRequest<IngestConfig>(
    `/api/v1/ingests/${encodeURIComponent(id)}/start`,
    {
      method: "POST",
    },
  );
}

async function stopIngest(id: string): Promise<IngestConfig | null> {
  return apiRequest<IngestConfig>(
    `/api/v1/ingests/${encodeURIComponent(id)}/stop`,
    {
      method: "POST",
    },
  );
}

async function getPipelineFileIngest(
  pipeId: string,
): Promise<PipelineFileIngestConfig | null> {
  return apiRequest<PipelineFileIngestConfig>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/file-ingest`,
  );
}

async function getMediaFileAnalysis(
  filename: string,
): Promise<MediaFileAnalysis | null> {
  return apiRequest<MediaFileAnalysis>(
    `/api/v1/media/${encodeURIComponent(filename)}/analysis`,
  );
}

async function logout(): Promise<{ ok: boolean } | null> {
  return apiRequest<{ ok: boolean }>("/api/v1/auth/logout", {
    method: "POST",
  });
}

async function changePassword(
  currentPassword: string,
  newPassword: string,
): Promise<{ ok: boolean } | null> {
  return apiRequest<{ ok: boolean }>("/api/v1/auth/change-password", {
    method: "POST",
    body: { currentPassword, newPassword },
  });
}

async function dismissPasswordChangePrompt(): Promise<{ ok: boolean } | null> {
  return apiRequest<{ ok: boolean }>("/api/v1/auth/dismiss-password-change", {
    method: "POST",
  });
}

async function getRateLimitState(): Promise<RateLimitState | null> {
  return apiRequest<RateLimitState>("/api/v1/security/rate-limits");
}

async function resetRateLimitState(body: {
  scope?: string;
  ip?: string;
}): Promise<{ ok: boolean; removed: number } | null> {
  return apiRequest<{ ok: boolean; removed: number }>(
    "/api/v1/security/rate-limits/reset",
    {
      method: "POST",
      body,
    },
  );
}

/** What YouTube's watch page says about a stream, fetched by the engine. */
async function getYoutubeMonitoringStatus(
  url: string,
): Promise<YoutubeMonitoringStatus | null> {
  return apiRequest<YoutubeMonitoringStatus>(
    `/api/v1/monitoring/youtube-status?url=${encodeURIComponent(url)}`,
  );
}

/** The engine's judgement of a change; nothing is mutated. */
async function planChange(body: ChangeRequest): Promise<ChangePlan | null> {
  return apiRequest<ChangePlan>("/api/v1/agent/plans", { method: "POST", body });
}

async function createOperation(body: ChangeRequest): Promise<ChangeOperation | null> {
  return apiRequest<ChangeOperation>("/api/v1/agent/operations", { method: "POST", body });
}

/** The stored record; a 404 means the engine restarted or evicted it. */
async function getOperation(operationId: string): Promise<ChangeOperation | null> {
  return apiRequest<ChangeOperation>(
    `/api/v1/agent/operations/${encodeURIComponent(operationId)}`,
  );
}

/** Approve (the dashboard session approves; `reason` is audited), apply, or
 * verify an operation. */
async function advanceOperation(
  operationId: string,
  step: "approve" | "apply" | "verify",
  reason?: string,
): Promise<ChangeOperation | null> {
  return apiRequest<ChangeOperation>(
    `/api/v1/agent/operations/${encodeURIComponent(operationId)}/${step}`,
    {
      method: "POST",
      body: step === "approve" ? { approvedBy: "control-room", reason: reason || null } : {},
    },
  );
}

export {
  apiRequest,
  getConfig,
  getSystemMetrics,
  getAggregateAlerts,
  getLifecycleEvents,
  getEngineTelemetry,
  getPipelineSummary,
  getFlightRecorder,
  getPipelineProbe,
  mintMoqPreview,
  getLogs,
  getStageTelemetry,
  getResourceMap,
  getCapacityScenario,
  getEngineStatus,
  getEngineHealth,
  getEngineSbomEndpoint,
  getEngineSbom,
  getAudioCapsPayload,
  runPipelineDiagnostics,
  buildLogsStreamUrl,
  createPipeline,
  getFleetHistory,
  getPipelineInputs,
  createPipelineInput,
  updatePipelineInput,
  deletePipelineInput,
  promotePipelineInput,
  updatePipeline,
  deletePipeline,
  createOutput,
  updateOutput,
  deleteOutput,
  startOut,
  stopOut,
  patchConfig,
  startRecording,
  stopRecording,
  listMediaFiles,
  mediaFileUrl,
  uploadMediaFile,
  deleteMediaFile,
  renameMediaFile,
  getPipelineFileIngest,
  startIngest,
  stopIngest,
  getMediaFileAnalysis,
  logout,
  changePassword,
  dismissPasswordChangePrompt,
  getRateLimitState,
  resetRateLimitState,
  getYoutubeMonitoringStatus,
  planChange,
  createOperation,
  getOperation,
  advanceOperation,
};

// Team, audit, notifications, schedules and share links.
export {
  getSession,
  setViewAs,
  listTeam,
  createTeamMember,
  updateTeamMember,
  deleteTeamMember,
  getAudit,
  listNotifyChannels,
  createNotifyChannel,
  updateNotifyChannel,
  deleteNotifyChannel,
  testNotifyChannel,
  getNotifyChannelSecret,
  listNotifyDeliveries,
  getPushPublicKey,
  listMyPushSubscriptions,
  savePushSubscription,
  deletePushSubscription,
  testPush,
  listSchedules,
  createSchedule,
  updateSchedule,
  deleteSchedule,
  runSchedule,
  listShareLinks,
  createShareLink,
  revokeShareLink,
} from "./api-admin.js";
