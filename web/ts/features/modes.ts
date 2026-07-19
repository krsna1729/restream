import { getRestreamHistory } from "../core/api.js";
import { createManagedLogStream } from "../core/log-stream.js";
import { escapeHtml } from "../core/utils.js";
import { state } from "../core/state.js";
import { renderControlRoom } from "./control-room.js";
import {
  refreshMediaLibraryMetricsOnly,
  renderMediaLibraryMode,
  resetMediaLibraryShellState,
} from "./media-library.js";
import { loadSettings, renderSettingsPanel } from "./settings.js";
import {
  loadStatus,
  setStatusStreamActive,
  syncStatusStreamVisibility,
} from "./status.js";
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
import type { AppLogRow } from "../types.js";
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
} from "../core/pipeline-workspace.js";
import type {
  DashboardLocation,
  DashboardMode,
  PipelineWorkspaceView,
} from "../core/pipeline-workspace.js";
import { syncPipelineWorkspaceShell } from "./pipeline-workspace-shell.js";
import { buildOverviewViewModel } from "./overview-view-model.js";
import type {
  OverviewMetricKey,
  OverviewPresentationInput,
  OverviewViewModel,
} from "./overview-view-model.js";

const runtimeDashboardModes = new Set<DashboardMode>(["overview", "pipeline"]);
let currentMode: DashboardMode | null = null;
let settingsMounted = false;
let statusMounted = false;
let inspectPanelShellHtml: string | null = null;
let controlPanelShellHtml: string | null = null;
const dashboardV2RouteChromeObservers = new WeakMap<
  HTMLElement,
  MutationObserver
>();
let lastPipelineWorkspaceHref: string | null = null;
type NavigationFocus = "preserve" | "panel";
interface NavigationOptions {
  focus?: NavigationFocus;
}
type StatusTone = "success" | "warning" | "error" | "neutral" | "info";
type SummaryCounts = ReturnType<typeof buildOverviewViewModel>["counts"];
type DashboardV2RouteBodyMode =
  | "pipeline-inspect"
  | "pipeline-monitor"
  | "incidents"
  | "telemetry"
  | "media"
  | "settings"
  | "status";

interface DashboardV2RouteBodyConfig {
  readonly bodyId: string;
  readonly mode: DashboardV2RouteBodyMode;
  readonly panelId: string;
  readonly rootId: string;
  readonly slotId: string;
}

const DASHBOARD_V2_ROUTE_BODIES: readonly DashboardV2RouteBodyConfig[] = [
  {
    bodyId: "inspect-mode-content",
    mode: "pipeline-inspect",
    panelId: "inspect-mode-panel",
    rootId: "dashboard-v2-pipeline-inspect-root",
    slotId: "dashboard-v2-pipeline-inspect-body-slot",
  },
  {
    bodyId: "control-mode-content",
    mode: "pipeline-monitor",
    panelId: "control-mode-panel",
    rootId: "dashboard-v2-control-room-root",
    slotId: "dashboard-v2-control-room-body-slot",
  },
  {
    bodyId: "incidents-mode-content",
    mode: "incidents",
    panelId: "incidents-mode-panel",
    rootId: "dashboard-v2-incidents-root",
    slotId: "dashboard-v2-incidents-body-slot",
  },
  {
    bodyId: "telemetry-mode-content",
    mode: "telemetry",
    panelId: "telemetry-mode-panel",
    rootId: "dashboard-v2-telemetry-root",
    slotId: "dashboard-v2-telemetry-body-slot",
  },
  {
    bodyId: "media-mode-content",
    mode: "media",
    panelId: "media-mode-panel",
    rootId: "dashboard-v2-media-root",
    slotId: "dashboard-v2-media-body-slot",
  },
  {
    bodyId: "settings-mode-content",
    mode: "settings",
    panelId: "settings-mode-panel",
    rootId: "dashboard-v2-settings-root",
    slotId: "dashboard-v2-settings-body-slot",
  },
  {
    bodyId: "status-mode-content",
    mode: "status",
    panelId: "status-mode-panel",
    rootId: "dashboard-v2-status-root",
    slotId: "dashboard-v2-status-body-slot",
  },
] as const;

const DASHBOARD_V2_ROUTE_OWNERSHIP_RETRY_LIMIT = 20;
const DASHBOARD_V2_ROUTE_OWNERSHIP_RETRY_MS = 50;

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
let legacyOverviewRenderEnabled = true;
let overviewPresentationHook:
  ((presentation: OverviewPresentationInput) => void) | null = null;
let dashboardModePresentationSync:
  | ((location: DashboardLocation) => void)
  | null = null;

export function configureDashboardModePresentationSync(
  callback: ((location: DashboardLocation) => void) | null,
): void {
  dashboardModePresentationSync = callback;
}

export function configureOverviewPresentation(options: {
  legacyRenderEnabled: boolean;
  onPresentation?: (presentation: OverviewPresentationInput) => void;
}): void {
  legacyOverviewRenderEnabled = options.legacyRenderEnabled;
  overviewPresentationHook = options.onPresentation || null;
  const legacyContainer = document.getElementById("overview-mode-content");
  if (legacyContainer) legacyContainer.hidden = !legacyOverviewRenderEnabled;
  if (!legacyOverviewRenderEnabled) {
    legacyContainer?.replaceChildren();
  }
}

function currentOverviewPresentation(): OverviewPresentationInput {
  const activityBursts = buildRestreamActivityBursts(
    overviewActivityLogs,
  ).slice(-OVERVIEW_ACTIVITY_LIMIT);
  return {
    activityBursts,
    activityLoading:
      overviewActivityInFlight !== null && activityBursts.length === 0,
    metricHistory: {
      inputs: [...overviewMetricHistory.inputs],
      outputs: [...overviewMetricHistory.outputs],
      inputKbps: [...overviewMetricHistory.inputKbps],
      outputKbps: [...overviewMetricHistory.outputKbps],
      engineCpu: [...overviewMetricHistory.engineCpu],
      engineMemory: [...overviewMetricHistory.engineMemory],
    },
  };
}

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
    (overviewActivityFetchedAt === 0 ||
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
    .catch(() => {
      overviewActivityFetchedAt = Date.now();
    })
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
    ? '<div class="text-base-content/70 text-sm">Loading recent restream activity...</div>'
    : bursts.length === 0
      ? '<div class="text-base-content/70 text-sm">No recent restream-wide activity yet.</div>'
      : `<div class="space-y-2">${renderRestreamActivityCards(
          overviewActivityLogs,
          OVERVIEW_ACTIVITY_LIMIT,
        )}</div>`;

  return `<section class="dashboard-section">
        <div class="dashboard-section-header">
            <div>
                <h2 class="dashboard-section-title">Restream Activity</h2>
                <p class="dashboard-subtitle">Recent restream-wide event bursts, grouped for operator-friendly review.</p>
            </div>
            <button type="button" class="btn btn-sm btn-outline" id="overview-open-status-btn">Open Status</button>
        </div>
        <div class="p-4">${body}</div>
    </section>`;
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
        ${detail ? `<span class="text-base-content/75 font-normal">${escapeHtml(detail)}</span>` : ""}
    </span>`;
}

function overviewAttentionSection(model: OverviewViewModel): string {
  const issues = model.attention;

  const body = issues.length
    ? issues
        .map(
          (
            item,
          ) => `<article class="dashboard-card p-3">
            <div class="flex min-w-0 items-start justify-between gap-3">
              <div class="min-w-0">
                <h3 class="truncate font-semibold">${escapeHtml(item.pipelineName)}</h3>
                <p class="text-base-content/70 mt-1 text-xs">${escapeHtml(item.detail)}</p>
              </div>
              <span class="badge ${badgeClassForTone(item.status.tone)} shrink-0">${escapeHtml(item.status.label)}</span>
            </div>
            <div class="mt-3 flex flex-wrap gap-2">
              <button type="button" class="btn btn-xs btn-outline js-open-pipeline" data-overview-focus="attention-operate" data-pipeline-id="${escapeHtml(item.pipelineId)}">Operate</button>
              <button type="button" class="btn btn-xs btn-outline js-inspect-pipeline" data-overview-focus="attention-inspect" data-pipeline-id="${escapeHtml(item.pipelineId)}">Inspect</button>
            </div>
          </article>`,
        )
        .join("")
    : `<div class="dashboard-empty">${
        model.counts.pipelines === 0
          ? "Add a pipeline to begin monitoring inputs and destinations."
          : "No active incident-level issues. Runtime detail stays available under Status and Pipeline Inspect."
      }</div>`;
  const title = issues.length
    ? `${issues.length} pipeline${issues.length === 1 ? "" : "s"} needs attention`
    : model.counts.pipelines === 0
      ? "Ready for the first pipeline"
      : "Fleet is clear";
  const tone = issues.length
    ? "border-warning/35 bg-warning/5"
    : "border-success/25 bg-success/5";
  const priorityTone = issues.length ? "text-warning" : "text-success";
  const issueGrid = issues.length > 1 ? "lg:grid-cols-2" : "";

  return `<section id="overview-attention" aria-labelledby="overview-attention-title" class="${tone} rounded-lg border">
    <div class="border-base-content/10 flex flex-wrap items-start justify-between gap-3 border-b px-4 py-4">
      <div>
        <p class="${priorityTone} text-xs font-semibold uppercase tracking-wider">Current priority</p>
        <h2 id="overview-attention-title" class="mt-1 text-xl font-semibold">${title}</h2>
        <p class="text-base-content/80 mt-1 text-sm">Issues are ordered by upstream cause and severity.</p>
      </div>
      <button type="button" class="btn btn-sm btn-outline" id="overview-open-status-detail-btn">Runtime detail</button>
    </div>
    <div class="grid gap-3 p-4 ${issueGrid}">${body}</div>
  </section>`;
}

interface OverviewFocusBookmark {
  id: string;
  focusKey: string;
  pipelineId: string;
}

function captureOverviewFocus(
  container: HTMLElement,
): OverviewFocusBookmark | null {
  const active = document.activeElement;
  if (!(active instanceof HTMLElement) || !container.contains(active)) {
    return null;
  }
  return {
    id: active.id,
    focusKey: active.dataset.overviewFocus || "",
    pipelineId: active.dataset.pipelineId || "",
  };
}

function restoreOverviewFocus(
  container: HTMLElement,
  bookmark: OverviewFocusBookmark | null,
): void {
  if (!bookmark) return;
  let target: HTMLElement | null = bookmark.id
    ? document.getElementById(bookmark.id)
    : null;
  if (!target && bookmark.focusKey && bookmark.pipelineId) {
    target = container.querySelector<HTMLElement>(
      `[data-overview-focus="${CSS.escape(bookmark.focusKey)}"][data-pipeline-id="${CSS.escape(bookmark.pipelineId)}"]`,
    );
  }
  target?.focus({ preventScroll: true });
}

function renderOverview(): void {
  const container = document.getElementById("overview-mode-content");
  if (!container) return;
  refreshOverviewActivityIfStale();

  const counts = buildOverviewViewModel(state.pipelines).counts;
  recordOverviewMetricSamples(counts);
  const presentation = currentOverviewPresentation();
  overviewPresentationHook?.(presentation);
  if (!legacyOverviewRenderEnabled) return;
  const model = buildOverviewViewModel(
    state.pipelines,
    state.metrics,
    presentation,
  );
  const pipelineRows = model.pipelines
    .map(
      (
        pipe,
      ) => `<tr class="border-base-content/5 hover:bg-base-100/60 border-t">
                <td class="min-w-56 py-3">
                    <button type="button" class="group flex max-w-xs text-left js-open-pipeline" data-overview-focus="pipeline-row" data-pipeline-id="${escapeHtml(pipe.id)}">
                        <span class="group-hover:text-accent truncate font-semibold">${escapeHtml(pipe.name)}</span>
                    </button>
                </td>
                <td>${statusPill(pipe.health.label, pipe.health.tone, pipe.health.detail)}</td>
                <td>${statusPill(pipe.input.label, pipe.input.tone, pipe.input.detail)}</td>
                <td>${statusPill(pipe.outputs.label, pipe.outputs.tone, pipe.outputs.detail)}</td>
                <td>${statusPill(pipe.inputRate.label, pipe.inputRate.tone)}</td>
                <td>${statusPill(pipe.outputRate.label, pipe.outputRate.tone)}</td>
                <td>${statusPill(pipe.recording.label, pipe.recording.tone, pipe.recording.detail)}</td>
            </tr>`,
    )
    .join("");

  const markup = `
      <div class="space-y-4">
        <header class="flex flex-wrap items-end justify-between gap-3">
          <div>
            <p class="text-accent text-xs font-semibold uppercase tracking-[0.18em]">Live operations</p>
            <h1 class="mt-1 text-2xl font-semibold">Fleet overview</h1>
            <p class="text-base-content/70 mt-1 text-sm">See what needs action before scanning throughput and system load.</p>
          </div>
          <button type="button" class="btn btn-sm btn-primary" id="overview-add-pipeline-btn">Add Pipeline</button>
        </header>
        <div class="grid items-start gap-4 lg:grid-cols-[minmax(0,1.2fr)_minmax(22rem,0.8fr)]">
          ${overviewAttentionSection(model)}
          <aside id="overview-fleet-signals" aria-label="Fleet signals" class="border-base-content/10 bg-base-200/80 rounded-lg border p-3">
            <div class="mb-3 flex items-center justify-between gap-3 px-1">
              <div>
                <h2 class="text-sm font-semibold">Fleet signals</h2>
                <p class="text-base-content/70 mt-0.5 text-xs">Current snapshot and recent trend</p>
              </div>
              <span class="badge badge-outline">${counts.pipelines} pipeline${counts.pipelines === 1 ? "" : "s"}</span>
            </div>
            <div class="grid grid-cols-2 gap-2">
              ${model.metrics.map((metric) => overviewMetric(metric.label, metric.value, metric.note, metric.key)).join("")}
            </div>
          </aside>
        </div>
        <section id="overview-pipelines" aria-labelledby="overview-pipelines-title" class="dashboard-table-panel">
            <div class="dashboard-section-header">
                <div>
                    <h2 id="overview-pipelines-title" class="dashboard-section-title">All pipelines</h2>
                    <p class="dashboard-subtitle">Compare intent, runtime state, and data flow.</p>
                </div>
            </div>
            <div class="overflow-x-auto">
                <table class="table table-sm">
                    <thead class="text-base-content/70 bg-base-100/50 text-xs uppercase">
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
                    <tbody>${pipelineRows || '<tr><td colspan="7" class="text-base-content/70 px-4 py-6">No pipelines configured.</td></tr>'}</tbody>
                </table>
            </div>
        </section>
        ${overviewActivitySection()}
      </div>`;

  if (container.innerHTML === markup) return;
  const focusBookmark = captureOverviewFocus(container);
  container.innerHTML = markup;

  container
    .querySelectorAll<HTMLElement>(".js-open-pipeline")
    .forEach((button) => {
      button.onclick = () => {
        if (!button.dataset.pipelineId) return;
        selectPipeline(button.dataset.pipelineId);
        setDashboardMode("pipeline", { focus: "panel" });
      };
    });
  container
    .querySelectorAll<HTMLElement>(".js-inspect-pipeline")
    .forEach((button) => {
      button.onclick = () => {
        if (!button.dataset.pipelineId) return;
        openInspectGraph(button.dataset.pipelineId, { focus: "panel" });
      };
    });
  const addBtn = document.getElementById("overview-add-pipeline-btn");
  if (addBtn) addBtn.onclick = () => void window.addPipeBtn();
  const statusDetailBtn = document.getElementById(
    "overview-open-status-detail-btn",
  );
  if (statusDetailBtn) {
    statusDetailBtn.onclick = () =>
      setDashboardMode("status", { focus: "panel" });
  }
  const statusBtn = document.getElementById("overview-open-status-btn");
  if (statusBtn) {
    statusBtn.onclick = () => setDashboardMode("status", { focus: "panel" });
  }
  restoreOverviewFocus(container, focusBookmark);
}

function overviewMetric(
  label: string,
  value: string,
  note: string,
  historyKey: OverviewMetricKey,
): string {
  const tone = overviewMetricTone(historyKey);
  return `<section class="${tone.borderClass} dashboard-stat-card border-t-2">
        <div class="dashboard-kicker">${escapeHtml(label)}</div>
        <div class="mt-1 grid grid-cols-[minmax(0,max-content)_minmax(2.5rem,1fr)] items-end gap-2">
            <div class="min-w-0">${overviewMetricHero(value)}</div>
            <div class="min-w-0">${overviewSparkline(historyKey)}</div>
        </div>
        <div class="dashboard-muted mt-1 truncate" title="${escapeHtml(note)}">${escapeHtml(note)}</div>
    </section>`;
}

function overviewMetricHero(value: string): string {
  const trimmed = value.trim();
  if (!trimmed || trimmed === "--") {
    return '<span class="text-xl font-semibold tabular-nums">--</span>';
  }
  const compactUnit = trimmed.match(/^(-?\d+(?:\.\d+)?)(%)$/);
  const spacedUnit = trimmed.match(/^(.+?)\s+([A-Za-z][A-Za-z/]+)$/);
  const match = compactUnit || spacedUnit;
  if (!match) {
    return `<span class="text-xl font-semibold tabular-nums">${escapeHtml(trimmed)}</span>`;
  }
  return `<span class="inline-flex min-w-0 items-baseline gap-1">
        <span class="truncate text-xl font-semibold tabular-nums">${escapeHtml(match[1])}</span>
        <span class="text-base-content/70 shrink-0 text-xs font-semibold">${escapeHtml(match[2])}</span>
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
            <div class="dashboard-page-shell">
                <div class="flex flex-wrap items-end justify-between gap-3">
                    <div>
                        <h1 class="dashboard-title">Status</h1>
                        <p class="dashboard-subtitle">Runtime build, native libraries, and system details.</p>
                    </div>
                    <button type="button" class="btn btn-sm btn-outline" id="refresh-status-btn" aria-label="Refresh status data">Refresh</button>
                </div>
                <section class="dashboard-section p-5">
                    <h2 class="dashboard-section-title mb-4">Runtime</h2>
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

function refreshActiveMode(options: NavigationOptions = {}): void {
  renderDashboardModes();
  if (options.focus === "panel") focusActivePanel();
}

function scrollTabIntoView(tab: HTMLButtonElement | null): void {
  if (!tab || typeof tab.scrollIntoView !== "function") return;
  tab.scrollIntoView({
    block: "nearest",
    inline: "nearest",
  });
}

function activeDashboardPanel(): HTMLElement | null {
  return document.querySelector<HTMLElement>(
    '#dashboard-main [role="tabpanel"]:not(.hidden)',
  );
}

function focusActivePanel(): void {
  const panel = activeDashboardPanel();
  if (!panel) return;
  panel.tabIndex = -1;
  panel.focus({ preventScroll: true });
  panel.scrollIntoView({ block: "start" });
}

function dashboardV2ShellActive(): boolean {
  const toggle = document.getElementById("dashboard-ui-v2-toggle");
  if (toggle instanceof HTMLInputElement && toggle.checked) return true;
  try {
    return new URLSearchParams(window.location.search).get("ui") === "v2";
  } catch (_err) {
    return false;
  }
}

function dashboardV2RouteBodyMode(
  mode: DashboardMode,
  pipelineView: PipelineWorkspaceView,
): DashboardV2RouteBodyMode | null {
  if (mode === "pipeline" && pipelineView === "inspect")
    return "pipeline-inspect";
  if (mode === "pipeline" && pipelineView === "monitor")
    return "pipeline-monitor";
  if (
    mode === "incidents" ||
    mode === "telemetry" ||
    mode === "media" ||
    mode === "settings" ||
    mode === "status"
  ) {
    return mode;
  }
  return null;
}

function appendRouteBodyToPanel(config: DashboardV2RouteBodyConfig): void {
  const body = document.getElementById(config.bodyId);
  const panel = document.getElementById(config.panelId);
  if (!body || !panel || body.parentElement === panel) return;
  panel.appendChild(body);
}

function slotRouteBodyIntoV2Root(config: DashboardV2RouteBodyConfig): boolean {
  const body = document.getElementById(config.bodyId);
  const root = document.getElementById(config.rootId);
  const panel = document.getElementById(config.panelId);
  if (!body || !root || !panel) return false;
  if (root.parentElement !== panel) panel.prepend(root);
  const slot = root.querySelector<HTMLElement>(
    `[data-dashboard-v2-route-body-slot="${config.slotId}"]`,
  );
  if (!slot) return false;
  if (body.parentElement !== slot) slot.appendChild(body);
  return true;
}

function setRouteChromeSuppressed(body: HTMLElement, suppressed: boolean): void {
  body
    .querySelectorAll<HTMLElement>(
      [
        ":scope > .dashboard-page-shell > .flex:first-child h1",
        ":scope > .dashboard-page-shell > .flex:first-child p",
        ":scope > .dashboard-page-shell > p[role='status']",
        ":scope > .flex:first-child h1",
        ":scope > .flex:first-child p",
        ":scope > :first-child > header:first-child h1",
        ":scope > :first-child > header:first-child p",
        ":scope > :first-child > p[role='status']:first-of-type",
        ":scope > h1:first-child",
        ":scope > p[role='status']:first-of-type",
      ].join(","),
    )
    .forEach((element) => {
      element.hidden = suppressed;
      if (suppressed) {
        element.setAttribute("aria-hidden", "true");
      } else {
        element.removeAttribute("aria-hidden");
      }
    });
}

function syncRouteChromeSuppressionObserver(
  body: HTMLElement,
  active: boolean,
): void {
  const observer = dashboardV2RouteChromeObservers.get(body);
  if (!active) {
    observer?.disconnect();
    dashboardV2RouteChromeObservers.delete(body);
    return;
  }
  if (observer) return;
  const nextObserver = new MutationObserver(() => {
    setRouteChromeSuppressed(body, true);
  });
  nextObserver.observe(body, { childList: true, subtree: true });
  dashboardV2RouteChromeObservers.set(body, nextObserver);
}

function scheduleDashboardV2RouteBodyOwnershipSync(
  config: DashboardV2RouteBodyConfig,
  attempt = 0,
): void {
  window.setTimeout(() => {
    const location = resolveDashboardLocation(window.location.href);
    if (
      dashboardV2RouteBodyMode(location.mode, location.pipelineView) ===
      config.mode
    ) {
      const slotted = slotRouteBodyIntoV2Root(config);
      const body = document.getElementById(config.bodyId);
      if (body) {
        setRouteChromeSuppressed(body, true);
        syncRouteChromeSuppressionObserver(body, true);
      }
      if (!slotted && attempt < DASHBOARD_V2_ROUTE_OWNERSHIP_RETRY_LIMIT) {
        scheduleDashboardV2RouteBodyOwnershipSync(config, attempt + 1);
      }
    }
  }, DASHBOARD_V2_ROUTE_OWNERSHIP_RETRY_MS);
}

function syncDashboardV2RouteBodyOwnership(
  mode: DashboardMode,
  pipelineView: PipelineWorkspaceView,
): void {
  const activeMode = dashboardV2ShellActive()
    ? dashboardV2RouteBodyMode(mode, pipelineView)
    : null;
  for (const config of DASHBOARD_V2_ROUTE_BODIES) {
    const body = document.getElementById(config.bodyId);
    const active = activeMode === config.mode;
    if (active) {
      const slotted = slotRouteBodyIntoV2Root(config);
      if (!slotted) scheduleDashboardV2RouteBodyOwnershipSync(config);
    } else {
      appendRouteBodyToPanel(config);
    }
    if (body) {
      setRouteChromeSuppressed(body, active);
      syncRouteChromeSuppressionObserver(body, active);
    }
  }
}

function rememberPipelineWorkspaceLocation(href = window.location.href): void {
  try {
    const location = resolveDashboardLocation(href);
    if (location.mode !== "pipeline") return;
    lastPipelineWorkspaceHref = location.url.href;
  } catch (_err) {
    lastPipelineWorkspaceHref = null;
  }
}

function restoredPipelineWorkspaceUrl(): URL | null {
  if (!dashboardV2ShellActive() || !lastPipelineWorkspaceHref) return null;
  const currentUrl = new URL(window.location.href);
  if (currentUrl.searchParams.has("p")) return null;
  const restoredUrl = new URL(lastPipelineWorkspaceHref);
  const currentUi = currentUrl.searchParams.get("ui");
  if (currentUi) restoredUrl.searchParams.set("ui", currentUi);
  return restoredUrl;
}

function snapshotPanelShell(panelId: string): string | null {
  const panel = document.getElementById(panelId);
  return panel ? panel.innerHTML : null;
}

function restorePanelShell(panelId: string, html: string | null): void {
  if (html === null) return;
  const panel = document.getElementById(panelId);
  if (!panel) return;
  if (
    panel.childElementCount > 1 ||
    (panel.firstElementChild &&
      !panel.firstElementChild.id.startsWith("dashboard-v2-"))
  )
    return;
  panel.innerHTML = html;
}

function restorePipelineWorkspaceShell(view: PipelineWorkspaceView): void {
  if (view === "inspect") {
    restorePanelShell("inspect-mode-panel", inspectPanelShellHtml);
  }
  if (view === "monitor") {
    restorePanelShell("control-mode-panel", controlPanelShellHtml);
  }
}

function unmountInactiveV2PipelineWorkspace(
  previousMode: DashboardMode | null,
): void {
  if (currentMode === "pipeline") return;
  if (previousMode !== null && previousMode !== "pipeline") return;
  if (!dashboardV2ShellActive()) return;
  inspectPanelShellHtml ??= snapshotPanelShell("inspect-mode-panel");
  controlPanelShellHtml ??= snapshotPanelShell("control-mode-panel");
}

function unmountInactiveV2HeavyRoute(previousMode: DashboardMode | null): void {
  if (
    previousMode !== "incidents" &&
    previousMode !== "telemetry" &&
    previousMode !== "media" &&
    previousMode !== "settings" &&
    previousMode !== "status"
  )
    return;
  if (previousMode === currentMode) return;
  if (!dashboardV2ShellActive()) return;
  const contentIdByMode: Partial<Record<DashboardMode, string>> = {
    incidents: "incidents-mode-content",
    telemetry: "telemetry-mode-content",
    media: "media-mode-content",
    settings: "settings-mode-content",
    status: "status-mode-content",
  };
  const contentId = contentIdByMode[previousMode];
  if (contentId) document.getElementById(contentId)?.replaceChildren();
  if (previousMode === "media") resetMediaLibraryShellState();
  if (previousMode === "settings") settingsMounted = false;
  if (previousMode === "status") statusMounted = false;
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
  unmountInactiveV2HeavyRoute(previousMode);
  unmountInactiveV2PipelineWorkspace(previousMode);
  syncPipelineWorkspaceShell(mode, pipelineView);

  let activeModeButton: HTMLButtonElement | null = null;
  document
    .querySelectorAll<HTMLButtonElement>("[data-dashboard-mode]")
    .forEach((button) => {
      const active = button.dataset.dashboardMode === mode;
      button.classList.toggle("btn-accent", active);
      button.classList.toggle("btn-outline", !active);
      button.setAttribute("aria-selected", active ? "true" : "false");
      button.tabIndex = active ? 0 : -1;
      if (active) activeModeButton = button;
    });
  scrollTabIntoView(activeModeButton);

  const summary = document.getElementById("workspace-mode-summary");
  if (summary) {
    const counts = buildOverviewViewModel(state.pipelines).counts;
    const taskSummary =
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
    const ownership = dashboardV2ShellActive() ? "UI v2 owned" : "Legacy owned";
    summary.textContent = `${ownership} · ${taskSummary}`;
  }
  if (
    previousMode !== null &&
    previousMode !== mode &&
    !runtimeDashboardModes.has(previousMode) &&
    runtimeDashboardModes.has(mode)
  ) {
    void refreshDashboard();
  }
  if (mode === "pipeline" && pipelineView === "monitor") {
    restorePipelineWorkspaceShell(pipelineView);
    renderControlRoom();
  }
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
  syncDashboardV2RouteBodyOwnership(mode, pipelineView);
  syncDashboardPolling();
}

function pushDashboardUrl(url: URL): boolean {
  if (url.href === window.location.href) return false;
  window.history.pushState({}, "", url);
  return true;
}

function setModeUrl(mode: DashboardMode): void {
  const url = dashboardModeUrl(window.location.href, mode);
  pushDashboardUrl(url);
}

function tablistNavigationTarget(
  button: HTMLButtonElement,
  selector: string,
  key: string,
): HTMLButtonElement | null {
  const tablist = button.closest('[role="tablist"]');
  if (!tablist) return null;
  const tabs = Array.from(
    tablist.querySelectorAll<HTMLButtonElement>(selector),
  ).filter((tab) => !tab.disabled && tab.offsetParent !== null);
  if (tabs.length === 0) return null;
  const index = tabs.indexOf(button);
  if (key === "Home") return tabs[0];
  if (key === "End") return tabs[tabs.length - 1];
  if (index < 0) return null;
  if (key === "ArrowRight" || key === "ArrowDown") {
    return tabs[(index + 1) % tabs.length];
  }
  if (key === "ArrowLeft" || key === "ArrowUp") {
    return tabs[(index - 1 + tabs.length) % tabs.length];
  }
  return null;
}

function activateTabFromKeyboard(
  event: KeyboardEvent,
  button: HTMLButtonElement,
  selector: string,
): void {
  const target = tablistNavigationTarget(button, selector, event.key);
  if (!target) return;
  event.preventDefault();
  target.focus();
  scrollTabIntoView(target);
  target.click();
}

export function setDashboardMode(
  mode: string,
  options: NavigationOptions = {},
): void {
  if (mode === "inspect" || mode === "control") {
    setPipelineWorkspaceView(
      mode === "inspect" ? "inspect" : "monitor",
      undefined,
      options,
    );
    return;
  }
  const currentLocation = resolveDashboardLocation(window.location.href);
  if (currentLocation.mode === "pipeline") {
    rememberPipelineWorkspaceLocation(currentLocation.url.href);
  }
  const candidate = new URL(window.location.href);
  candidate.searchParams.set("mode", mode);
  candidate.searchParams.delete("view");
  const nextMode = resolveDashboardLocation(candidate.href).mode;
  if (nextMode === "pipeline" && currentLocation.mode === "pipeline") {
    applyMode(nextMode, currentLocation.pipelineView);
    if (options.focus === "panel") focusActivePanel();
    return;
  }
  const restoredPipelineUrl =
    nextMode === "pipeline" ? restoredPipelineWorkspaceUrl() : null;
  if (restoredPipelineUrl) {
    pushDashboardUrl(restoredPipelineUrl);
  } else {
    setModeUrl(nextMode);
  }
  if (currentMode === nextMode) {
    const location = resolveDashboardLocation(window.location.href);
    applyMode(nextMode, location.pipelineView);
    if (options.focus === "panel") focusActivePanel();
    return;
  }
  refreshActiveMode(options);
}

export function setPipelineWorkspaceView(
  view: PipelineWorkspaceView,
  pipelineId?: string | null,
  options: NavigationOptions = {},
): void {
  const url = pipelineWorkspaceUrl(window.location.href, view, pipelineId);
  pushDashboardUrl(url);
  rememberPipelineWorkspaceLocation(url.href);
  refreshActiveMode(options);
}

export function openInspectGraph(
  pipeId: string,
  options: NavigationOptions = {},
): void {
  resetPipelineInspectorSelection(pipeId);
  setPipelineWorkspaceView("inspect", pipeId, options);
}

export function renderDashboardModes(): void {
  const location = canonicalizeDashboardLocation();
  if (location.mode === "pipeline") {
    rememberPipelineWorkspaceLocation(location.url.href);
  }
  dashboardModePresentationSync?.(location);
  if (location.mode === "overview") renderOverview();
  if (location.mode === "pipeline" && location.pipelineView === "inspect") {
    restorePipelineWorkspaceShell(location.pipelineView);
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
      button.onkeydown = (event) =>
        activateTabFromKeyboard(event, button, "[data-dashboard-mode]");
    });
  document
    .querySelectorAll<HTMLButtonElement>("[data-pipeline-workspace-view]")
    .forEach((button) => {
      button.onclick = () =>
        setPipelineWorkspaceView(
          (button.dataset.pipelineWorkspaceView ||
            "operate") as PipelineWorkspaceView,
        );
      button.onkeydown = (event) =>
        activateTabFromKeyboard(
          event,
          button,
          "[data-pipeline-workspace-view]",
        );
    });
  window.addEventListener("popstate", () => refreshActiveMode());
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
  document.addEventListener("dashboard:v2-route-rendered", () => {
    const location = resolveDashboardLocation(window.location.href);
    syncDashboardV2RouteBodyOwnership(location.mode, location.pipelineView);
  });
  window.setDashboardMode = setDashboardMode;
  refreshActiveMode();
}
