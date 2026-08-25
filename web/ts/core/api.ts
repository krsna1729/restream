import { showLoading, hideLoading, showErrorAlert } from "./utils.js";
import { withBasePath } from "./base-path.js";
import { redirectToLogin } from "./auth-redirect.js";
import type {
  ConfigOutput,
  ConfigPipeline,
  ConfigData,
  BackendPolicy,
  HealthData,
  IngestSecurityConfig,
  OutputConfig,
  RecordingSettings,
  SrtGlobalIngestConfig,
  SrtPipelineIngestConfig,
  DashboardRuntimeSnapshot,
  DiagnosticsReport,
  PipelineInputMutationResponse,
  PipelineInputPromotionResponse,
  PipelineInputsResponse,
  SystemMetrics,
  StreamKey,
} from "../types.js";
import type {
  OverviewSnapshot,
  AlertsSnapshot,
  LifecycleEventsSnapshot,
  EngineTelemetrySnapshot,
  PipelineTelemetrySnapshot,
  StageTelemetrySnapshot,
  ResourceMapSnapshot,
  PipelineSummarySnapshot,
  BuildLogsStreamUrlOptions,
  PipelineMutationResponse,
  OutputMutationResponse,
  OutputMutationArgs,
  MediaFile,
  IngestConfig,
  PipelineFileIngestConfig,
  MediaFileAnalysis,
  TranscodeProfiles,
  YoutubeMonitoringStatus,
  RateLimitAttempt,
  RateLimitState,
} from "./api-types.js";

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

async function parseJsonResponse<T>(response: Response): Promise<T | null> {
  try {
    return (await response.json()) as T;
  } catch (e) {
    showErrorAlert("Invalid JSON response: " + e);
    return null;
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
  return apiRequest<ConfigData>(url);
}

interface GetHealthOptions {
  view?: "full" | "summary";
}

async function getHealth(
  options: GetHealthOptions = {},
): Promise<HealthData | null> {
  const query = new URLSearchParams();
  if (options.view === "summary") query.set("view", "summary");
  const suffix = query.toString();
  const url = suffix
    ? `/api/v1/engine/health?${suffix}`
    : "/api/v1/engine/health";
  return apiRequest<HealthData>(url);
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

interface GetDashboardRuntimeOptions {
  healthView?: "full" | "summary";
  metricsView?: "full" | "summary";
  pipelineId?: string | null;
}

async function getDashboardRuntimeSnapshot(
  options: GetDashboardRuntimeOptions = {},
): Promise<DashboardRuntimeSnapshot | null> {
  const query = new URLSearchParams();
  if (options.healthView) query.set("health_view", options.healthView);
  if (options.metricsView) query.set("metrics_view", options.metricsView);
  if (options.pipelineId) query.set("pipeline_id", options.pipelineId);
  const suffix = query.toString();
  const url = suffix
    ? `/api/v1/dashboard/runtime?${suffix}`
    : "/api/v1/dashboard/runtime";
  return apiRequest<DashboardRuntimeSnapshot>(url);
}



async function getOverview(): Promise<OverviewSnapshot | null> {
  return apiRequest<OverviewSnapshot>("/api/v1/overview");
}

async function getAggregateAlerts(): Promise<AlertsSnapshot | null> {
  return apiRequest<AlertsSnapshot>("/api/v1/alerts");
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

async function getPipelineTelemetry(
  pipelineId: string,
): Promise<PipelineTelemetrySnapshot | null> {
  return apiRequest<PipelineTelemetrySnapshot>(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/telemetry`,
  );
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

async function getPipelineSummary(
  pipelineId: string,
): Promise<PipelineSummarySnapshot | null> {
  return apiRequest<PipelineSummarySnapshot>(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/summary`,
  );
}

async function getStreamKeys(): Promise<StreamKey[] | null> {
  return apiRequest<StreamKey[]>("/api/v1/stream-keys");
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

interface CreatePipelineArgs {
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

async function createPipeline(
  args: CreatePipelineArgs,
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
  label: string,
): Promise<PipelineInputMutationResponse | null> {
  return apiRequest<PipelineInputMutationResponse>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/inputs`,
    { method: "POST", body: { label } },
  );
}

async function updatePipelineInput(
  pipeId: string,
  inputId: string,
  data: { label?: string; enabled?: boolean },
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
  data: unknown,
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

interface GetOutputHistoryOptions {
  limit?: number;
  filter?: string | null;
  since?: string | null;
  until?: string | null;
  order?: string | null;
  prefixes?: string[] | null;
}

async function getOutputHistory(
  pipeId: string,
  outId: string,
  options: GetOutputHistoryOptions = {},
): Promise<{ logs: unknown[] } | null> {
  if (!pipeId || !outId) {
    showErrorAlert("Pipeline id and output id are required");
    return null;
  }

  const {
    limit = 200,
    filter = null,
    since = null,
    until = null,
    order = null,
    prefixes = null,
  } = options;

  const query = new URLSearchParams();
  query.set("pipeline_id", pipeId);
  query.set("output_id", outId);

  if (filter === "lifecycle") {
    query.set("event_class", "lifecycle");
  } else {
    const safeLimit = Number.isFinite(Number(limit)) ? Number(limit) : 200;
    query.set("limit", String(safeLimit));
  }

  if (since) query.set("since", String(since));
  if (until) query.set("until", String(until));
  if (order) query.set("order", String(order));
  if (Array.isArray(prefixes) && prefixes.length > 0) {
    query.set("prefix", prefixes.join(","));
  }

  const res = await apiRequest<{ logs: Record<string, unknown>[] }>(
    `/api/v1/logs?${query.toString()}`,
  );
  if (!res) return null;
  return res;
}

async function getPipelineHistory(
  pipeId: string,
  limit = 400,
): Promise<{ logs: unknown[] } | null> {
  if (!pipeId) {
    showErrorAlert("Pipeline id is required");
    return null;
  }

  const safeLimit = Number.isFinite(Number(limit)) ? Number(limit) : 200;
  const query = new URLSearchParams({
    pipeline_id: pipeId,
    limit: String(safeLimit),
  });

  const res = await apiRequest<{ logs: Record<string, unknown>[] }>(
    `/api/v1/logs?${query.toString()}`,
  );
  if (!res) return null;
  return res;
}

interface GetRestreamHistoryOptions {
  limit?: number;
  order?: string | null;
  filter?: string | null;
}

async function getRestreamHistory(
  options: GetRestreamHistoryOptions = {},
): Promise<{ logs: unknown[] } | null> {
  const { limit = 200, order = null, filter = null } = options;
  const safeLimit = Number.isFinite(Number(limit)) ? Number(limit) : 200;
  const query = new URLSearchParams({
    scope: "restream",
    limit: String(safeLimit),
  });

  if (order) query.set("order", String(order));
  if (filter === "lifecycle") {
    query.set("event_class", "lifecycle");
  }

  const res = await apiRequest<{ logs: Record<string, unknown>[] }>(
    `/api/v1/logs?${query.toString()}`,
  );
  if (!res) return null;
  return res;
}

async function patchConfig(body: {
  serverName?: string;
  ingestHost?: string;
  ingestSecurity?: Partial<IngestSecurityConfig>;
  recordingSettings?: RecordingSettings;
  srtIngest?: SrtGlobalIngestConfig;
  backendPolicy?: BackendPolicy;
  transcodeProfiles?: TranscodeProfiles;
}): Promise<{
  serverName: string;
  ingestHost: string;
  dashboardPasswordChangeRecommended?: boolean;
  ingestSecurity: IngestSecurityConfig;
  recordingSettings: RecordingSettings;
  srtIngest: SrtGlobalIngestConfig;
  backendPolicy: BackendPolicy;
  transcodeProfiles?: TranscodeProfiles;
} | null> {
  return apiRequest<{
    serverName: string;
    ingestHost: string;
    dashboardPasswordChangeRecommended?: boolean;
    ingestSecurity: IngestSecurityConfig;
    recordingSettings: RecordingSettings;
    srtIngest: SrtGlobalIngestConfig;
    backendPolicy: BackendPolicy;
    transcodeProfiles?: TranscodeProfiles;
  }>("/api/v1/settings", { method: "PATCH", body });
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

async function listIngests(): Promise<IngestConfig[] | null> {
  return apiRequest<IngestConfig[]>("/api/v1/ingests");
}

async function createIngest(data: {
  filename: string;
  streamKey: string;
  loop: boolean;
  startTime: string;
}): Promise<IngestConfig | null> {
  return apiRequest<IngestConfig>("/api/v1/ingests", {
    method: "POST",
    body: data,
  });
}

async function updateIngest(
  id: string,
  data: {
    filename: string;
    streamKey: string;
    loop: boolean;
    startTime: string;
  },
): Promise<IngestConfig | null> {
  return apiRequest<IngestConfig>(`/api/v1/ingests/${encodeURIComponent(id)}`, {
    method: "PUT",
    body: data,
  });
}

async function deleteIngest(id: string): Promise<{ deleted: boolean } | null> {
  return apiRequest<{ deleted: boolean }>(
    `/api/v1/ingests/${encodeURIComponent(id)}`,
    {
      method: "DELETE",
    },
  );
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

async function putPipelineFileIngest(
  pipeId: string,
  data: {
    filename: string;
    loopFlag: boolean;
    startTime: string;
    liveOptimized: boolean;
    targetGopSeconds: number;
  },
): Promise<PipelineFileIngestConfig | null> {
  return apiRequest<PipelineFileIngestConfig>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/file-ingest`,
    { method: "PUT", body: data },
  );
}

async function getMediaFileAnalysis(
  filename: string,
): Promise<MediaFileAnalysis | null> {
  return apiRequest<MediaFileAnalysis>(
    `/api/v1/media/${encodeURIComponent(filename)}/analysis`,
  );
}

async function deletePipelineFileIngest(
  pipeId: string,
): Promise<{ deleted: boolean } | null> {
  return apiRequest<{ deleted: boolean }>(
    `/api/v1/pipelines/${encodeURIComponent(pipeId)}/file-ingest`,
    { method: "DELETE" },
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

async function getProcessingGraph(pipelineId: string): Promise<unknown | null> {
  return apiRequest(
    `/api/v1/pipelines/${encodeURIComponent(pipelineId)}/graph`,
  );
}

async function getYoutubeMonitoringStatus(
  url: string,
): Promise<YoutubeMonitoringStatus | null> {
  return apiRequest<YoutubeMonitoringStatus>(
    `/api/v1/monitoring/youtube-status?url=${encodeURIComponent(url)}`,
  );
}

export {
  apiRequest,
  getConfig,
  getHealth,
  getSystemMetrics,
  getDashboardRuntimeSnapshot,
  getOverview,
  getAggregateAlerts,
  getLifecycleEvents,
  getEngineTelemetry,
  getPipelineTelemetry,
  getPipelineSummary,
  getStageTelemetry,
  getResourceMap,
  getStreamKeys,
  getEngineStatus,
  getEngineHealth,
  getEngineSbomEndpoint,
  getEngineSbom,
  getAudioCapsPayload,
  runPipelineDiagnostics,
  buildLogsStreamUrl,
  createPipeline,
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
  getOutputHistory,
  getPipelineHistory,
  getRestreamHistory,
  patchConfig,
  startRecording,
  stopRecording,
  listMediaFiles,
  uploadMediaFile,
  deleteMediaFile,
  renameMediaFile,
  listIngests,
  createIngest,
  updateIngest,
  deleteIngest,
  startIngest,
  stopIngest,
  getPipelineFileIngest,
  putPipelineFileIngest,
  getMediaFileAnalysis,
  deletePipelineFileIngest,
  logout,
  changePassword,
  dismissPasswordChangePrompt,
  getRateLimitState,
  resetRateLimitState,
  getProcessingGraph,
  getYoutubeMonitoringStatus,
  DEFAULT_ENGINE_SBOM_ENDPOINT,
};

// Re-export all public types from api-types for backward compatibility.
// New code should import types from "./api-types.js" directly when only types are needed.
export type {
  OverviewSnapshot,
  AlertSeverity,
  AlertScope,
  OperatorAlert,
  AlertsSnapshot,
  LifecycleEvent,
  LifecycleEventsSnapshot,
  TelemetryMetrics,
  TelemetryIngest,
  TelemetryStage,
  TelemetryEgress,
  SourceRingReader,
  SourceRingTelemetry,
  EngineTelemetrySnapshot,
  PipelineTelemetrySnapshot,
  StageTelemetrySnapshot,
  ResourceMapMemory,
  ResourceMapNode,
  ResourceMapSnapshot,
  PipelineSummarySnapshot,
  BuildLogsStreamUrlOptions,
  PipelineMutationResponse,
  OutputMutationResponse,
  OutputMutationArgs,
  TranscodeProfile,
  TranscodeProfiles,
  MediaFile,
  IngestConfig,
  PipelineFileIngestConfig,
  MediaFileAnalysis,
  AudioCapsPayload,
  YoutubeMonitoringStatus,
  RateLimitAttempt,
  RateLimitState,
} from "./api-types.js";
