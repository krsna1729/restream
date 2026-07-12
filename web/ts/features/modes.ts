import { getRestreamHistory } from "../core/api.js";
import { createManagedLogStream } from "../core/log-stream.js";
import { escapeHtml } from "../core/utils.js";
import { state } from "../core/state.js";
import { renderControlRoom } from "./control-room.js";
import {
  refreshMediaLibraryMetricsOnly,
  renderMediaLibraryMode,
} from "./media-library.js";
import { loadSettings, renderSettingsPanel } from "./settings.js";
import {
  loadStatus,
  setStatusStreamActive,
  syncStatusStreamVisibility,
} from "./status.js";
import {
  isOutputFlapping,
  isOutputIntentStopped,
  isOutputRunning,
  isOutputRetrying,
  isOutputUnexpectedlyDown,
} from "../core/output-status.js";
import { selectPipeline } from "./render.js";
import {
  buildRestreamActivityBursts,
  renderRestreamActivityCards,
} from "./overview-activity.js";
import {
  handleDashboardRuntimeLifecycleLog,
  refreshDashboard,
  refreshDashboardRuntime,
  requestDetailedMetricsRefresh,
  syncDashboardPolling,
  syncDashboardRuntimeStream,
} from "./dashboard.js";
import type { AppLogRow, OutputView, PipelineView } from "../types.js";
import { renderIncidentsMode } from "./incidents.js";
import { renderEngineerTelemetryMode } from "./engineer-telemetry.js";
import {
  renderPipelineInspector,
  resetPipelineInspectorSelection,
  syncPipelineInspectorVisibility,
} from "./pipeline-inspector.js";
import {
  canonicalizeDashboardLocation,
  dashboardModeUrl,
  pipelineWorkspaceUrl,
  resolveDashboardLocation,
  syncPipelineWorkspaceShell,
} from "../app/pipeline-workspace.js";
import type {
  DashboardMode,
  PipelineWorkspaceView,
} from "../app/pipeline-workspace.js";

const runtimeDashboardModes = new Set<DashboardMode>(["overview", "pipeline"]);
let currentMode: DashboardMode | null = null;
let settingsMounted = false;
let statusMounted = false;
type StatusTone = "success" | "warning" | "error" | "neutral" | "info";
type OverviewMetricKey =
  | "inputs"
  | "outputs"
  | "inputKbps"
  | "outputKbps"
  | "engineCpu"
  | "engineMemory";
type SummaryCounts = ReturnType<typeof summaryCounts>;

const OVERVIEW_HISTORY_LIMIT = 28;
const OVERVIEW_ACTIVITY_LIMIT = 6;
const OVERVIEW_ACTIVITY_FETCH_LIMIT = 24;
const OVERVIEW_ACTIVITY_STALE_MS = 15_000;
const overviewMetricHistory: Record<OverviewMetricKey, number[]> = {
  inputs: [],
  outputs: [],
  inputKbps: [],
  outputKbps: [],
  engineCpu: [],
  engineMemory: [],
};
let lastOverviewMetricsSampleKey: string | null = null;
let overviewActivityLogs: AppLogRow[] = [];
let overviewActivityFetchedAt = 0;
let overviewActivityInFlight: Promise<void> | null = null;
const overviewActivityStream = createManagedLogStream();
let overviewActivityStreamActive = false;

function overviewActivityLogKey(log: AppLogRow): string {
  const id = Number(log?.id);
  if (Number.isFinite(id) && id > 0) return `id:${id}`;
  return `msg:${String(log?.ts || "")}:${String(log?.target || "")}:${String(log?.message || "")}`;
}

function setOverviewActivityLogs(logs: AppLogRow[]): void {
  const deduped = new Map<string, AppLogRow>();
  for (const log of Array.isArray(logs) ? logs : []) {
    deduped.set(overviewActivityLogKey(log), log);
  }
  overviewActivityLogs = [...deduped.values()]
    .sort((a, b) => Date.parse(b.ts || "") - Date.parse(a.ts || ""))
    .slice(0, OVERVIEW_ACTIVITY_FETCH_LIMIT);
  overviewActivityFetchedAt = Date.now();
}

function mergeOverviewActivityLogs(logs: AppLogRow[]): void {
  if (!Array.isArray(logs) || logs.length === 0) return;
  setOverviewActivityLogs([...logs, ...overviewActivityLogs]);
}

function latestOverviewActivityId(): number | null {
  const ids = overviewActivityLogs
    .map((log) => Number(log?.id))
    .filter((value) => Number.isFinite(value) && value > 0);
  return ids.length > 0 ? Math.max(...ids) : null;
}

function closeOverviewActivityStream(): void {
  overviewActivityStream.close();
  overviewActivityStreamActive = false;
}

function overviewActivityStreamingEnabled(): boolean {
  return (
    !document.hidden && (currentMode === null || currentMode === "overview")
  );
}

function ensureOverviewActivityStream(): void {
  if (!overviewActivityStreamingEnabled()) {
    closeOverviewActivityStream();
    return;
  }
  overviewActivityStreamActive = typeof EventSource === "function";
  overviewActivityStream.sync({
    filters: {
      scope: "restream",
    },
    resumeAfterId: latestOverviewActivityId(),
    onLog: (data) => {
      mergeOverviewActivityLogs([data]);
      if (
        data.eventClass === "lifecycle" ||
        (!data.eventClass && Boolean(data.eventType))
      ) {
        handleDashboardRuntimeLifecycleLog(data);
      }
      if (currentMode === "overview" || currentMode === null) renderOverview();
    },
    onUnavailable: () => {
      overviewActivityStreamActive = false;
      // The current snapshot remains authoritative while SSE is unavailable.
      // Start a fresh staleness window so renderOverview() cannot spin through
      // immediate fetch -> reconnect-failure cycles.
      overviewActivityFetchedAt = Date.now();
    },
  });
}

function refreshOverviewActivityIfStale(): void {
  if (!overviewActivityStreamingEnabled()) {
    closeOverviewActivityStream();
    return;
  }
  const shouldFetchSnapshot =
    !overviewActivityStreamActive &&
    (overviewActivityLogs.length === 0 ||
      Date.now() - overviewActivityFetchedAt >= OVERVIEW_ACTIVITY_STALE_MS);
  if (!shouldFetchSnapshot) {
    ensureOverviewActivityStream();
    return;
  }
  if (overviewActivityInFlight) return;

  overviewActivityInFlight = (async () => {
    const res = await getRestreamHistory({
      limit: OVERVIEW_ACTIVITY_FETCH_LIMIT,
      order: "desc",
    });
    if (res && Array.isArray(res.logs)) {
      setOverviewActivityLogs(res.logs as AppLogRow[]);
    }
  })()
    .catch(() => {})
    .finally(() => {
      overviewActivityInFlight = null;
      ensureOverviewActivityStream();
      if (currentMode === "overview" || currentMode === null) renderOverview();
    });
}

export function syncOverviewActivityStream(): void {
  if (!overviewActivityStreamingEnabled()) {
    closeOverviewActivityStream();
    return;
  }
  refreshOverviewActivityIfStale();
}

function overviewActivitySection(): string {
  const bursts = buildRestreamActivityBursts(overviewActivityLogs).slice(
    -OVERVIEW_ACTIVITY_LIMIT,
  );
  const loading = overviewActivityInFlight !== null && bursts.length === 0;
  const body = loading
    ? '<div class="text-base-content/60 text-sm">Loading recent restream activity...</div>'
    : bursts.length === 0
      ? '<div class="text-base-content/60 text-sm">No recent restream-wide activity yet.</div>'
      : `<div class="space-y-2">${renderRestreamActivityCards(
          overviewActivityLogs,
          OVERVIEW_ACTIVITY_LIMIT,
        )}</div>`;

  return `<section class="border-base-content/10 bg-base-200/80 rounded-lg border">
        <div class="border-base-content/10 flex flex-wrap items-center justify-between gap-2 border-b px-4 py-3">
            <div>
                <h2 class="text-base font-semibold">Restream Activity</h2>
                <p class="text-base-content/60 mt-1 text-sm">Recent restream-wide event bursts, grouped for operator-friendly review.</p>
            </div>
            <button type="button" class="btn btn-sm btn-outline" id="overview-open-status-btn">Open Status</button>
        </div>
        <div class="p-4">${body}</div>
    </section>`;
}

function formatBitrate(kbps: number | null | undefined): string {
  if (!Number.isFinite(kbps as number) || (kbps as number) < 0) return "--";
  const value = kbps as number;
  return value >= 1000
    ? `${(value / 1000).toFixed(1)} Mb/s`
    : `${value.toFixed(0)} Kb/s`;
}

function formatBytes(bytes: number | null | undefined): string {
  if (!Number.isFinite(bytes as number) || (bytes as number) <= 0) return "--";
  const value = bytes as number;
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  if (value < 1024 * 1024 * 1024)
    return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function formatPercent(value: number | null | undefined): string {
  if (!Number.isFinite(value as number) || (value as number) < 0) return "--";
  return `${(value as number).toFixed((value as number) >= 10 ? 0 : 1)}%`;
}

function hasMetricValue(value: number | null | undefined): boolean {
  return Number.isFinite(value as number) && (value as number) >= 0;
}

function joinMetricDetails(parts: string[], fallback = "warming..."): string {
  return parts.filter((part) => part.trim().length > 0).join(" / ") || fallback;
}

function pushOverviewMetric(
  key: OverviewMetricKey,
  value: number | null | undefined,
): void {
  if (!Number.isFinite(value as number)) return;
  const history = overviewMetricHistory[key];
  history.push(Math.max(0, value as number));
  if (history.length > OVERVIEW_HISTORY_LIMIT)
    history.splice(0, history.length - OVERVIEW_HISTORY_LIMIT);
}

function recordOverviewMetricSamples(counts: SummaryCounts): void {
  if (!state.metrics.generatedAt) return;
  const engineMemory =
    state.metrics.engine?.totalMemoryBytes ?? state.metrics.engine?.memoryBytes;
  const sampleKey = state.metrics.generatedAt;
  if (sampleKey === lastOverviewMetricsSampleKey) return;
  lastOverviewMetricsSampleKey = sampleKey;

  pushOverviewMetric("inputs", counts.liveInputs);
  pushOverviewMetric("outputs", counts.runningOutputs);
  pushOverviewMetric("inputKbps", counts.inputKbps);
  pushOverviewMetric("outputKbps", counts.outputKbps);
  if (state.metrics.engine?.cpuSampleReady !== false) {
    pushOverviewMetric("engineCpu", state.metrics.engine?.cpuPercent);
  }
  pushOverviewMetric("engineMemory", engineMemory);
}

function overviewSparkline(key: OverviewMetricKey): string {
  const values = overviewMetricHistory[key];
  if (values.length < 2) return "";
  const tone = overviewMetricTone(key);
  const min = Math.min(...values);
  const max = Math.max(...values);
  const rawRange = max - min;
  const midpoint = (max + min) / 2;
  const stableRange = Math.max(Math.abs(midpoint) * 0.05, 1);
  const points = values
    .map((value, index) => {
      const x = values.length === 1 ? 0 : (index / (values.length - 1)) * 100;
      const y =
        rawRange < stableRange
          ? 20 - ((value - midpoint) / stableRange) * 16
          : 36 - ((value - min) / rawRange) * 32;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return `<svg class="${tone.sparklineClass} h-12 w-full opacity-70" viewBox="0 0 100 40" preserveAspectRatio="none" aria-hidden="true">
        <polyline fill="none" stroke="currentColor" stroke-width="2.5" vector-effect="non-scaling-stroke" points="${points}"></polyline>
    </svg>`;
}

function overviewMetricTone(key: OverviewMetricKey): {
  borderClass: string;
  sparklineClass: string;
} {
  switch (key) {
    case "engineCpu":
      return {
        borderClass: "border-t-warning",
        sparklineClass: "text-warning",
      };
    case "engineMemory":
      return { borderClass: "border-t-info", sparklineClass: "text-info" };
    case "inputs":
      return {
        borderClass: "border-t-success",
        sparklineClass: "text-success",
      };
    case "outputs":
      return {
        borderClass: "border-t-secondary",
        sparklineClass: "text-secondary",
      };
    case "inputKbps":
      return { borderClass: "border-t-accent", sparklineClass: "text-accent" };
    case "outputKbps":
      return {
        borderClass: "border-t-primary",
        sparklineClass: "text-primary",
      };
  }
}

function badgeClassForTone(tone: StatusTone): string {
  if (tone === "success") return "badge-success";
  if (tone === "warning") return "badge-warning";
  if (tone === "error") return "badge-error";
  if (tone === "info") return "badge-info";
  return "badge-neutral";
}

function statusPill(label: string, tone: StatusTone, detail?: string): string {
  const toneClass =
    tone === "success"
      ? "border-success/30 bg-success/10 text-success"
      : tone === "warning"
        ? "border-warning/35 bg-warning/10 text-warning"
        : tone === "error"
          ? "border-error/35 bg-error/10 text-error"
          : tone === "info"
            ? "border-info/30 bg-info/10 text-info"
            : "border-base-content/10 bg-base-100/80 text-base-content/75";
  return `<span class="${toneClass} inline-flex min-h-8 max-w-full items-center gap-2 rounded-lg border px-2.5 py-1 text-xs font-semibold leading-tight">
        <span class="truncate">${escapeHtml(label)}</span>
        ${detail ? `<span class="text-base-content/55 font-normal">${escapeHtml(detail)}</span>` : ""}
    </span>`;
}

function pipelineHealthLabel(pipe: PipelineView): {
  label: string;
  cls: string;
  tone: StatusTone;
  detail?: string;
} {
  if (pipe.input.status === "error") {
    return {
      label: "Input error",
      cls: badgeClassForTone("error"),
      tone: "error",
      detail: "publisher fault",
    };
  }
  if (pipe.input.status === "warning") {
    return {
      label: pipe.input.flapping ? "Input flapping" : "Input warning",
      cls: badgeClassForTone("warning"),
      tone: "warning",
      detail: pipe.input.flapping
        ? `${Math.max(pipe.input.recentDisconnectCount, 2)} recent drops`
        : "check ingest",
    };
  }
  if (pipe.input.status !== "on") {
    if (pipe.outs.some(isOutputUnexpectedlyDown)) {
      return {
        label: "Input down",
        cls: badgeClassForTone("error"),
        tone: "error",
        detail: "outputs blocked",
      };
    }
    return {
      label: "Idle",
      cls: badgeClassForTone("neutral"),
      tone: "neutral",
      detail: "waiting for input",
    };
  }
  if (!pipe.input.probeReady) {
    return {
      label: "Input probing",
      cls: badgeClassForTone("warning"),
      tone: "warning",
      detail: "waiting for stream metadata",
    };
  }
  if (pipe.outs.some(isOutputUnexpectedlyDown)) {
    return {
      label: "Output down",
      cls: badgeClassForTone("error"),
      tone: "error",
      detail: "input live",
    };
  }
  if (pipe.outs.some(isOutputRetrying)) {
    return {
      label: "Output retrying",
      cls: badgeClassForTone("warning"),
      tone: "warning",
      detail: "recovering",
    };
  }
  if (pipe.outs.some(isOutputFlapping)) {
    return {
      label: "Output flapping",
      cls: badgeClassForTone("warning"),
      tone: "warning",
      detail: "recent sink drops",
    };
  }
  if (pipe.outs.some((out) => out.status === "warning")) {
    return {
      label: "Output warning",
      cls: badgeClassForTone("warning"),
      tone: "warning",
      detail: "input live",
    };
  }
  if (pipe.input.flapping) {
    return {
      label: "Input flapping",
      cls: badgeClassForTone("warning"),
      tone: "warning",
      detail: `${Math.max(pipe.input.recentDisconnectCount, 2)} recent drops`,
    };
  }
  return {
    label: "Live",
    cls: badgeClassForTone("success"),
    tone: "success",
    detail: "healthy",
  };
}

function outputStateLabel(out: OutputView): { label: string; cls: string } {
  if (isOutputIntentStopped(out))
    return { label: "Stopped", cls: "badge-neutral" };
  if (out.status === "failed") return { label: "Failed", cls: "badge-error" };
  if (out.status === "stalled")
    return { label: "Stalled", cls: "badge-warning" };
  if (isOutputRetrying(out)) return { label: "Retrying", cls: "badge-warning" };
  if (isOutputFlapping(out)) return { label: "Flapping", cls: "badge-warning" };
  if (isOutputRunning(out)) return { label: "Running", cls: "badge-success" };
  if (out.status === "warning")
    return { label: "Warning", cls: "badge-warning" };
  return { label: "Down", cls: "badge-error" };
}

function summaryCounts() {
  const outputs = state.pipelines.flatMap((pipe) => pipe.outs);
  return {
    pipelines: state.pipelines.length,
    liveInputs: state.pipelines.filter(
      (pipe) =>
        pipe.input.status === "on" &&
        pipe.input.probeReady &&
        !pipe.input.flapping,
    ).length,
    warningInputs: state.pipelines.filter(
      (pipe) =>
        pipe.input.status === "warning" ||
        (pipe.input.status === "on" &&
          (!pipe.input.probeReady || pipe.input.flapping)),
    ).length,
    runningOutputs: outputs.filter(isOutputRunning).length,
    retryingOutputs: outputs.filter(isOutputRetrying).length,
    flappingOutputs: outputs.filter(isOutputFlapping).length,
    stoppedOutputs: outputs.filter(isOutputIntentStopped).length,
    downOutputs: outputs.filter(isOutputUnexpectedlyDown).length,
    recording: state.pipelines.filter((pipe) => pipe.recording.active).length,
    inputKbps: state.pipelines.reduce(
      (sum, pipe) => sum + (pipe.stats.inputBitrateKbps || 0),
      0,
    ),
    outputKbps: state.pipelines.reduce(
      (sum, pipe) => sum + (pipe.stats.outputBitrateKbps || 0),
      0,
    ),
  };
}

function overviewAttentionSection(): string {
  const issues = state.pipelines
    .map((pipe) => {
      const health = pipelineHealthLabel(pipe);
      const down = pipe.outs.filter(isOutputUnexpectedlyDown).length;
      const retrying = pipe.outs.filter(isOutputRetrying).length;
      const flapping = pipe.outs.filter(isOutputFlapping).length;
      const warnings = pipe.outs.filter(
        (output) => output.status === "warning",
      ).length;
      const count = down + retrying + flapping + warnings;
      const detailParts = [
        down ? `${down} down` : "",
        retrying ? `${retrying} retrying` : "",
        flapping ? `${flapping} flapping` : "",
        warnings ? `${warnings} warning` : "",
        pipe.input.flapping
          ? `${Math.max(pipe.input.recentDisconnectCount, 2)} input drops`
          : "",
      ].filter(Boolean);
      const needsAttention =
        health.tone === "error" ||
        health.tone === "warning" ||
        count > 0 ||
        pipe.input.status === "error" ||
        pipe.input.status === "warning";
      if (!needsAttention) return null;
      return {
        pipe,
        health,
        count,
        detail: detailParts.join(" / ") || health.detail || "check pipeline",
      };
    })
    .filter((item): item is NonNullable<typeof item> => item !== null)
    .sort((left, right) => {
      const severity =
        Number(right.health.tone === "error") -
        Number(left.health.tone === "error");
      return (
        severity ||
        right.count - left.count ||
        left.pipe.name.localeCompare(right.pipe.name)
      );
    })
    .slice(0, 4);

  const body = issues.length
    ? issues
        .map(
          ({ pipe, health, detail }) => `<article class="border-base-content/10 bg-base-100 rounded-lg border p-3">
            <div class="flex min-w-0 items-start justify-between gap-3">
              <div class="min-w-0">
                <h3 class="truncate font-semibold">${escapeHtml(pipe.name)}</h3>
                <p class="text-base-content/60 mt-1 text-xs">${escapeHtml(detail)}</p>
              </div>
              <span class="badge ${health.cls} shrink-0">${escapeHtml(health.label)}</span>
            </div>
            <div class="mt-3 flex flex-wrap gap-2">
              <button type="button" class="btn btn-xs btn-outline js-open-pipeline" data-pipeline-id="${escapeHtml(pipe.id)}">Operate</button>
              <button type="button" class="btn btn-xs btn-outline js-inspect-pipeline" data-pipeline-id="${escapeHtml(pipe.id)}">Inspect</button>
            </div>
          </article>`,
        )
        .join("")
    : `<div class="border-base-content/10 bg-base-100 rounded-lg border p-4 text-sm text-base-content/70">No active incident-level issues. Runtime detail stays available under Status and Pipeline Inspect.</div>`;

  return `<section class="border-base-content/10 bg-base-200/80 mb-4 rounded-lg border">
    <div class="border-base-content/10 flex flex-wrap items-center justify-between gap-2 border-b px-4 py-3">
      <div>
        <h2 class="text-base font-semibold">Needs attention</h2>
        <p class="text-base-content/60 mt-1 text-sm">Fleet incidents are folded into Overview, then handled from the affected pipeline.</p>
      </div>
      <button type="button" class="btn btn-sm btn-outline" id="overview-open-status-detail-btn">Runtime detail</button>
    </div>
    <div class="grid gap-3 p-4 lg:grid-cols-2">${body}</div>
  </section>`;
}

function inputOverviewPill(pipe: PipelineView): string {
  const protocol = pipe.input.publisher?.protocol?.toUpperCase();
  const rate = formatBitrate(pipe.stats.inputBitrateKbps);
  if (pipe.input.status === "on" && !pipe.input.probeReady) {
    const pendingMs = pipe.input.probePendingMs;
    const detail =
      Number.isFinite(pendingMs as number) && (pendingMs as number) > 0
        ? `${protocol || "publisher"} / ${(Number(pendingMs) / 1000).toFixed(1)}s`
        : protocol || "publisher";
    return statusPill("Input probing", "warning", detail);
  }
  if (pipe.input.status === "on") {
    if (pipe.input.flapping) {
      return statusPill(
        "Input flapping",
        "warning",
        `${Math.max(pipe.input.recentDisconnectCount, 2)} recent drops${protocol ? ` / ${protocol}` : ""}`,
      );
    }
    return statusPill(
      "Live input",
      "success",
      [protocol, rate !== "--" ? rate : null].filter(Boolean).join(" / "),
    );
  }
  if (pipe.input.status === "warning") {
    return statusPill(
      pipe.input.flapping ? "Input flapping" : "Input warning",
      "warning",
      pipe.input.flapping
        ? `${Math.max(pipe.input.recentDisconnectCount, 2)} recent drops`
        : protocol || "publisher attached",
    );
  }
  if (pipe.input.status === "error") {
    return statusPill("Input error", "error", protocol || "publisher fault");
  }
  return statusPill(
    "No input",
    "neutral",
    pipe.inputSource ? "file/source idle" : "waiting",
  );
}

function outputsOverviewPill(pipe: PipelineView): string {
  const total = pipe.outs.length;
  const running = pipe.outs.filter(isOutputRunning).length;
  const retrying = pipe.outs.filter(isOutputRetrying).length;
  const flapping = pipe.outs.filter(isOutputFlapping).length;
  const stopped = pipe.outs.filter(isOutputIntentStopped).length;
  const down = pipe.outs.filter(isOutputUnexpectedlyDown).length;
  if (!total) return statusPill("No outputs", "neutral", "not configured");
  if (pipe.input.status !== "on" && down > 0) {
    return statusPill(
      `${running}/${total} running`,
      "neutral",
      "blocked by input",
    );
  }
  if (down > 0)
    return statusPill(`${down} down`, "error", `${running}/${total} running`);
  if (retrying > 0) {
    return statusPill(
      `${retrying} retrying`,
      "warning",
      `${running}/${total} running`,
    );
  }
  if (flapping > 0) {
    return statusPill(
      `${flapping} flapping`,
      "warning",
      `${running}/${total} running`,
    );
  }
  if (stopped === total)
    return statusPill("Stopped", "neutral", `${total} configured`);
  if (running === total)
    return statusPill(`${running}/${total} running`, "success");
  return statusPill(
    `${running}/${total} running`,
    "warning",
    `${stopped} stopped`,
  );
}

function recordingOverviewPill(pipe: PipelineView): string {
  if (pipe.recording.active) return statusPill("Recording", "error", "active");
  if (pipe.recording.enabled) return statusPill("Armed", "warning", "ready");
  return statusPill("Off", "neutral");
}

function rateOverviewPill(kbps: number | null | undefined): string {
  const value = formatBitrate(kbps);
  return statusPill(value, value === "--" ? "neutral" : "info");
}

function renderOverview(): void {
  const container = document.getElementById("overview-mode-content");
  if (!container) return;
  refreshOverviewActivityIfStale();

  const counts = summaryCounts();
  recordOverviewMetricSamples(counts);
  const engine = state.metrics.engine || {};
  const ffmpegCount = Number(engine.externalFfmpegCount || 0);
  const ffmpegMemory = Number(engine.externalFfmpegMemoryBytes || 0);
  const restreamMemory = Number(
    engine.restreamMemoryBytes ?? engine.memoryBytes ?? 0,
  );
  const engineMemory = Number(
    engine.totalMemoryBytes || restreamMemory + ffmpegMemory,
  );
  const engineCpuDetail = joinMetricDetails([
    hasMetricValue(engine.restreamCpuPercent)
      ? `Restream ${formatPercent(engine.restreamCpuPercent)}`
      : "",
    ffmpegCount > 0 && hasMetricValue(engine.externalFfmpegCpuPercent)
      ? `FFmpeg ${formatPercent(engine.externalFfmpegCpuPercent)} (${ffmpegCount})`
      : "",
  ]);
  const engineMemoryDetail = joinMetricDetails(
    [
      hasMetricValue(restreamMemory) && restreamMemory > 0
        ? `Restream ${formatBytes(restreamMemory)}`
        : "",
      ffmpegCount > 0 && hasMetricValue(ffmpegMemory) && ffmpegMemory > 0
        ? `FFmpeg ${formatBytes(ffmpegMemory)}`
        : "",
    ],
    "No engine memory sample",
  );
  const pipelineRows = [...state.pipelines]
    .sort((a, b) => a.name.localeCompare(b.name))
    .map((pipe) => {
      const health = pipelineHealthLabel(pipe);
      return `<tr class="border-base-content/5 hover:bg-base-100/60 border-t">
                <td class="min-w-56 py-3">
                    <button type="button" class="group flex max-w-xs text-left js-open-pipeline" data-pipeline-id="${escapeHtml(pipe.id)}">
                        <span class="group-hover:text-accent truncate font-semibold">${escapeHtml(pipe.name)}</span>
                    </button>
                </td>
                <td>${statusPill(health.label, health.tone, health.detail)}</td>
                <td>${inputOverviewPill(pipe)}</td>
                <td>${outputsOverviewPill(pipe)}</td>
                <td>${rateOverviewPill(pipe.stats.inputBitrateKbps)}</td>
                <td>${rateOverviewPill(pipe.stats.outputBitrateKbps)}</td>
                <td>${recordingOverviewPill(pipe)}</td>
            </tr>`;
    })
    .join("");

  container.innerHTML = `
        <div class="mb-4 grid gap-3 md:grid-cols-3">
            ${overviewMetric("Engine CPU", formatPercent(engine.cpuPercent), engineCpuDetail, "engineCpu")}
            ${overviewMetric("Inputs Live", `${counts.liveInputs}/${counts.pipelines}`, counts.warningInputs ? `${counts.warningInputs} warning` : "All quiet", "inputs")}
            ${overviewMetric("Throughput In", formatBitrate(counts.inputKbps), "Across active publishers", "inputKbps")}
            ${overviewMetric("Engine Memory", formatBytes(engineMemory), engineMemoryDetail, "engineMemory")}
            ${overviewMetric("Outputs Running", `${counts.runningOutputs}`, `${counts.retryingOutputs} retrying / ${counts.flappingOutputs} flapping / ${counts.downOutputs} down / ${counts.stoppedOutputs} stopped`, "outputs")}
            ${overviewMetric("Throughput Out", formatBitrate(counts.outputKbps), `${counts.recording} active recording${counts.recording === 1 ? "" : "s"}`, "outputKbps")}
        </div>
        ${overviewAttentionSection()}
        <section class="border-base-content/10 bg-base-200/80 rounded-lg border">
            <div class="border-base-content/10 flex flex-wrap items-center justify-between gap-2 border-b px-4 py-3">
                <div>
                    <h1 class="text-lg font-semibold">Operator Overview</h1>
                    <p class="text-base-content/60 text-sm">Primary state follows the upstream cause before downstream symptoms.</p>
                </div>
                <button type="button" class="btn btn-sm btn-outline" id="overview-add-pipeline-btn">Add Pipeline</button>
            </div>
            <div class="overflow-x-auto">
                <table class="table table-sm">
                    <thead class="text-base-content/55 bg-base-100/50 text-xs uppercase">
                        <tr>
                            <th>Pipeline</th>
                            <th>State</th>
                            <th>Input</th>
                            <th>Outputs</th>
                            <th>Input Rate</th>
                            <th>Output Rate</th>
                            <th>Recording</th>
                        </tr>
                    </thead>
                    <tbody>${pipelineRows || '<tr><td colspan="7" class="text-base-content/60 px-4 py-6">No pipelines configured.</td></tr>'}</tbody>
                </table>
            </div>
        </section>
        ${overviewActivitySection()}`;

  container
    .querySelectorAll<HTMLElement>(".js-open-pipeline")
    .forEach((button) => {
      button.onclick = () => {
        if (!button.dataset.pipelineId) return;
        selectPipeline(button.dataset.pipelineId);
        setDashboardMode("pipeline");
      };
    });
  container
    .querySelectorAll<HTMLElement>(".js-inspect-pipeline")
    .forEach((button) => {
      button.onclick = () => {
        if (!button.dataset.pipelineId) return;
        openInspectGraph(button.dataset.pipelineId);
      };
    });
  const addBtn = document.getElementById("overview-add-pipeline-btn");
  if (addBtn) addBtn.onclick = () => void window.addPipeBtn();
  const statusDetailBtn = document.getElementById(
    "overview-open-status-detail-btn",
  );
  if (statusDetailBtn) {
    statusDetailBtn.onclick = () => setDashboardMode("status");
  }
  const statusBtn = document.getElementById("overview-open-status-btn");
  if (statusBtn) {
    statusBtn.onclick = () => setDashboardMode("status");
  }
}

function overviewMetric(
  label: string,
  value: string,
  note: string,
  historyKey: OverviewMetricKey,
): string {
  const tone = overviewMetricTone(historyKey);
  return `<section class="${tone.borderClass} border-base-content/10 bg-base-200 min-h-30 overflow-hidden rounded-lg border border-t-2 p-4">
        <div class="text-base-content/60 text-xs font-semibold uppercase">${escapeHtml(label)}</div>
        <div class="mt-2 grid grid-cols-[minmax(0,max-content)_minmax(5rem,1fr)] items-end gap-3">
            <div class="min-w-0">${overviewMetricHero(value)}</div>
            <div class="min-w-0">${overviewSparkline(historyKey)}</div>
        </div>
        <div class="text-base-content/60 mt-1 text-sm">${escapeHtml(note)}</div>
    </section>`;
}

function overviewMetricHero(value: string): string {
  const trimmed = value.trim();
  if (!trimmed || trimmed === "--") {
    return '<span class="text-2xl font-semibold tabular-nums">--</span>';
  }
  const compactUnit = trimmed.match(/^(-?\d+(?:\.\d+)?)(%)$/);
  const spacedUnit = trimmed.match(/^(.+?)\s+([A-Za-z][A-Za-z/]+)$/);
  const match = compactUnit || spacedUnit;
  if (!match) {
    return `<span class="text-2xl font-semibold tabular-nums">${escapeHtml(trimmed)}</span>`;
  }
  return `<span class="inline-flex min-w-0 items-baseline gap-1">
        <span class="truncate text-2xl font-semibold tabular-nums">${escapeHtml(match[1])}</span>
        <span class="text-base-content/55 shrink-0 text-sm font-semibold">${escapeHtml(match[2])}</span>
    </span>`;
}
function renderSettingsMode(): void {
  const container = document.getElementById("settings-mode-content");
  if (!container) return;
  if (!settingsMounted || !container.querySelector("#settings-server-name")) {
    renderSettingsPanel(container);
    settingsMounted = true;
    void loadSettings({ embedded: true });
  }
}

function renderStatusMode(): void {
  const container = document.getElementById("status-mode-content");
  if (!container) return;
  if (!statusMounted || !container.querySelector("#status-versions")) {
    container.innerHTML = `
            <div class="mx-auto max-w-5xl space-y-5">
                <div class="flex flex-wrap items-end justify-between gap-3">
                    <div>
                        <h1 class="text-lg font-semibold">Status</h1>
                        <p class="text-base-content/60 mt-1 text-sm">Runtime build, native libraries, and system details.</p>
                    </div>
                    <button type="button" class="btn btn-sm btn-outline" id="refresh-status-btn">Refresh</button>
                </div>
                <section class="border-base-content/10 bg-base-200 rounded-lg border p-5">
                    <h2 class="mb-4 text-base font-semibold">Runtime</h2>
                    <div id="status-versions" class="space-y-5">
                        <p class="text-sm opacity-60">Loading...</p>
                    </div>
                </section>
            </div>`;
    container
      .querySelector<HTMLButtonElement>("#refresh-status-btn")
      ?.addEventListener("click", () => void loadStatus());
    statusMounted = true;
    void loadStatus();
  }
}

function refreshActiveMode(): void {
  renderDashboardModes();
}

function applyMode(
  mode: DashboardMode,
  pipelineView: PipelineWorkspaceView,
): void {
  const previousMode = currentMode;
  currentMode = mode;
  syncOverviewActivityStream();
  setStatusStreamActive(mode === "status");
  syncDashboardRuntimeStream();
  const panels: Record<
    Exclude<DashboardMode, "pipeline">,
    HTMLElement | null
  > = {
    overview: document.getElementById("overview-mode-panel"),
    incidents: document.getElementById("incidents-mode-panel"),
    telemetry: document.getElementById("telemetry-mode-panel"),
    media: document.getElementById("media-mode-panel"),
    settings: document.getElementById("settings-mode-panel"),
    status: document.getElementById("status-mode-panel"),
  };
  for (const [name, panel] of Object.entries(panels)) {
    panel?.classList.toggle("hidden", name !== mode);
  }
  syncPipelineWorkspaceShell(mode, pipelineView);

  document
    .querySelectorAll<HTMLButtonElement>("[data-dashboard-mode]")
    .forEach((button) => {
      const active = button.dataset.dashboardMode === mode;
      button.classList.toggle("btn-accent", active);
      button.classList.toggle("btn-outline", !active);
      button.setAttribute("aria-selected", active ? "true" : "false");
      button.tabIndex = active ? 0 : -1;
    });

  const summary = document.getElementById("workspace-mode-summary");
  if (summary) {
    const counts = summaryCounts();
    summary.textContent =
      mode === "overview"
        ? `${counts.liveInputs} live inputs / ${counts.runningOutputs} running outputs${counts.retryingOutputs ? ` / ${counts.retryingOutputs} retrying` : ""}${counts.flappingOutputs ? ` / ${counts.flappingOutputs} flapping` : ""}`
        : mode === "pipeline"
          ? pipelineView === "inspect"
            ? "Pipeline graph and diagnostics"
            : pipelineView === "monitor"
              ? "Pipeline monitoring wall"
              : "Pipeline workflow"
          : mode === "incidents"
            ? "Alerts, evidence, and lifecycle events"
            : mode === "telemetry"
              ? "Engine and pipeline counters"
              : mode === "media"
                ? "Recordings and source files"
                : mode === "settings"
                  ? "Server configuration"
                  : "Runtime status";
  }
  if (
    previousMode !== null &&
    previousMode !== mode &&
    !runtimeDashboardModes.has(previousMode) &&
    runtimeDashboardModes.has(mode)
  ) {
    void refreshDashboard();
  }
  if (mode === "pipeline" && pipelineView === "monitor") renderControlRoom();
  const pipelineOptions = state.pipelines.map((pipeline) => ({
    id: pipeline.id,
    name: pipeline.name || pipeline.id,
  }));
  renderIncidentsMode({
    active: mode === "incidents",
    pipelines: pipelineOptions,
    navigateToPipeline: (pipelineId) => {
      selectPipeline(pipelineId);
      setDashboardMode("pipeline");
    },
  });
  renderEngineerTelemetryMode({
    active: mode === "telemetry",
    pipelines: pipelineOptions,
  });
  if (mode === "media") {
    if (previousMode !== "media") {
      requestDetailedMetricsRefresh();
      void refreshDashboardRuntime();
      void renderMediaLibraryMode();
    } else {
      refreshMediaLibraryMetricsOnly();
    }
  }
  if (mode === "settings") renderSettingsMode();
  if (mode === "status") renderStatusMode();
  syncDashboardPolling();
}

function setModeUrl(mode: DashboardMode): void {
  const url = dashboardModeUrl(window.location.href, mode);
  window.history.pushState({}, "", url);
}

export function setDashboardMode(mode: string): void {
  if (mode === "inspect" || mode === "control") {
    setPipelineWorkspaceView(mode === "inspect" ? "inspect" : "monitor");
    return;
  }
  const candidate = new URL(window.location.href);
  candidate.searchParams.set("mode", mode);
  candidate.searchParams.delete("view");
  const nextMode = resolveDashboardLocation(candidate.href).mode;
  setModeUrl(nextMode);
  if (currentMode === nextMode) {
    applyMode(nextMode, "operate");
    return;
  }
  refreshActiveMode();
}

export function setPipelineWorkspaceView(
  view: PipelineWorkspaceView,
  pipelineId?: string | null,
): void {
  const url = pipelineWorkspaceUrl(window.location.href, view, pipelineId);
  window.history.pushState({}, "", url);
  refreshActiveMode();
}

export function openInspectGraph(pipeId: string): void {
  selectPipeline(pipeId);
  resetPipelineInspectorSelection(pipeId);
  setPipelineWorkspaceView("inspect", pipeId);
}

export function renderDashboardModes(): void {
  const location = canonicalizeDashboardLocation();
  if (location.mode === "overview") renderOverview();
  if (location.mode === "pipeline" && location.pipelineView === "inspect") {
    renderPipelineInspector();
  }
  applyMode(location.mode, location.pipelineView);
}

export function initDashboardModes(): void {
  document
    .querySelectorAll<HTMLButtonElement>("[data-dashboard-mode]")
    .forEach((button) => {
      button.onclick = () =>
        setDashboardMode(button.dataset.dashboardMode || "overview");
    });
  document
    .querySelectorAll<HTMLButtonElement>("[data-pipeline-workspace-view]")
    .forEach((button) => {
      button.onclick = () =>
        setPipelineWorkspaceView(
          (button.dataset.pipelineWorkspaceView ||
            "operate") as PipelineWorkspaceView,
        );
    });
  window.addEventListener("popstate", refreshActiveMode);
  document.addEventListener("visibilitychange", () => {
    syncOverviewActivityStream();
    syncStatusStreamVisibility();
    if (
      !document.hidden &&
      resolveDashboardLocation(window.location.href).mode === "pipeline" &&
      resolveDashboardLocation(window.location.href).pipelineView === "inspect"
    ) {
      syncPipelineInspectorVisibility();
    }
  });
  window.setDashboardMode = setDashboardMode;
  refreshActiveMode();
}
