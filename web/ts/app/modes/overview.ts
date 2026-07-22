import { getRestreamHistory } from "../../core/api.js";
import { createManagedLogStream } from "../../core/log-stream.js";
import { state } from "../../core/state.js";
import { escapeHtml } from "../../core/utils.js";
import { selectPipeline } from "../../features/render.js";
import {
  buildRestreamActivityBursts,
  renderRestreamActivityCards,
} from "../../features/overview-activity.js";
import { handleDashboardRuntimeLifecycleLog } from "../../features/dashboard.js";
import type { AppLogRow } from "../../types.js";
import type {
  OverviewMetricKey,
  OverviewPresentationInput,
  OverviewViewModel,
} from "../../features/overview-view-model.js";
import { buildOverviewViewModel } from "../../features/overview-view-model.js";

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

let overviewActivityLogs: AppLogRow[] = [];
let overviewActivityFetchedAt = 0;
let overviewActivityInFlight: Promise<void> | null = null;
const overviewActivityStream = createManagedLogStream();
let overviewActivityStreamActive = false;
let legacyOverviewRenderEnabled = true;
let overviewPresentationHook:
  | ((presentation: OverviewPresentationInput) => void)
  | null = null;
let lastOverviewMetricsSampleKey: string | null = null;

export function configureOverviewPresentation(options: {
  legacyRenderEnabled?: boolean;
  onPresentation?: (presentation: OverviewPresentationInput) => void;
  onStateChange?: (model: OverviewViewModel | null) => void;
}): void {
  if (typeof options.legacyRenderEnabled === "boolean") {
    legacyOverviewRenderEnabled = options.legacyRenderEnabled;
  }
  overviewPresentationHook = options.onPresentation || null;
  const legacyContainer = document.getElementById("overview-mode-content");
  if (legacyContainer) legacyContainer.hidden = !legacyOverviewRenderEnabled;
  if (!legacyOverviewRenderEnabled) {
    legacyContainer?.replaceChildren();
  }
}

export function currentOverviewPresentation(): OverviewPresentationInput {
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
  return !document.hidden;
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
      renderOverview();
    },
    onUnavailable: () => {
      overviewActivityStreamActive = false;
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
      renderOverview();
    });
}

export function renderOverviewActivity(): void {
  const presentation = currentOverviewPresentation();
  if (overviewPresentationHook) {
    overviewPresentationHook(presentation);
  }
  const listEl =
    document.getElementById("overview-activity-list") ||
    document.getElementById("overview-mode-content");
  if (listEl && legacyOverviewRenderEnabled) {
    const markup = renderRestreamActivityCards(
      overviewActivityLogs,
      OVERVIEW_ACTIVITY_LIMIT,
    );
    if (listEl.innerHTML !== markup) listEl.innerHTML = markup;
  }
}

function pushOverviewMetric(
  key: OverviewMetricKey,
  value: number | null | undefined,
): void {
  if (!Number.isFinite(value as number)) return;
  const history = overviewMetricHistory[key];
  history.push(Math.max(0, value as number));
  if (history.length > 28) history.splice(0, history.length - 28);
}

function recordOverviewMetricSamples(
  counts: OverviewViewModel["counts"],
): void {
  if (!state.metrics.generatedAt) return;
  const sampleKey = state.metrics.generatedAt;
  if (sampleKey === lastOverviewMetricsSampleKey) return;
  lastOverviewMetricsSampleKey = sampleKey;
  const engineMemory =
    state.metrics.engine?.totalMemoryBytes ?? state.metrics.engine?.memoryBytes;
  pushOverviewMetric("inputs", counts.liveInputs);
  pushOverviewMetric("outputs", counts.runningOutputs);
  pushOverviewMetric("inputKbps", counts.inputKbps);
  pushOverviewMetric("outputKbps", counts.outputKbps);
  if (state.metrics.engine?.cpuSampleReady !== false) {
    pushOverviewMetric("engineCpu", state.metrics.engine?.cpuPercent);
  }
  pushOverviewMetric("engineMemory", engineMemory);
}

function badgeClassForTone(tone: string): string {
  if (tone === "success") return "badge-success";
  if (tone === "warning") return "badge-warning";
  if (tone === "error") return "badge-error";
  if (tone === "info") return "badge-info";
  return "badge-neutral";
}

function statusPill(label: string, tone: string, detail?: string): string {
  return `<span class="badge ${badgeClassForTone(tone)} gap-1">${escapeHtml(label)}${detail ? ` <span class="font-normal">${escapeHtml(detail)}</span>` : ""}</span>`;
}

function overviewAttentionSection(model: OverviewViewModel): string {
  const body = model.attention.length
    ? model.attention
        .map(
          (item) => `<article class="dashboard-card p-3">
            <div class="flex min-w-0 items-start justify-between gap-3">
              <div class="min-w-0">
                <h3 class="truncate font-semibold">${escapeHtml(item.pipelineName)}</h3>
                <p class="text-base-content/70 mt-1 text-xs">${escapeHtml(item.detail)}</p>
              </div>
              ${statusPill(item.status.label, item.status.tone, item.status.detail)}
            </div>
            <div class="mt-3 flex flex-wrap gap-2">
              <button type="button" class="btn btn-xs btn-outline js-open-pipeline" data-pipeline-id="${escapeHtml(item.pipelineId)}">Operate</button>
              <button type="button" class="btn btn-xs btn-outline js-inspect-pipeline" data-pipeline-id="${escapeHtml(item.pipelineId)}">Inspect</button>
            </div>
          </article>`,
        )
        .join("")
    : `<div class="dashboard-empty">${
        model.counts.pipelines === 0
          ? "Add a pipeline to begin monitoring inputs and destinations."
          : "No active incident-level issues. Runtime detail stays available under Status and Pipeline Inspect."
      }</div>`;
  const title = model.attention.length
    ? `${model.attention.length} pipeline${model.attention.length === 1 ? "" : "s"} needs attention`
    : model.counts.pipelines === 0
      ? "Ready for the first pipeline"
      : "Fleet is clear";
  return `<section id="overview-attention" class="dashboard-section p-4">
    <div class="dashboard-section-header">
      <div>
        <h2 class="dashboard-section-title">${title}</h2>
        <p class="dashboard-subtitle">Issues are ordered by upstream cause and severity.</p>
      </div>
      <button type="button" class="btn btn-sm btn-outline" id="overview-open-status-detail-btn">Runtime detail</button>
    </div>
    <div class="grid gap-3">${body}</div>
  </section>`;
}

function overviewMetric(metric: OverviewViewModel["metrics"][number]): string {
  return `<section class="dashboard-stat-card">
    <div class="dashboard-kicker">${escapeHtml(metric.label)}</div>
    <div class="mt-1 text-xl font-semibold tabular-nums">${escapeHtml(metric.value)}</div>
    <div class="dashboard-muted mt-1 truncate" title="${escapeHtml(metric.note)}">${escapeHtml(metric.note)}</div>
  </section>`;
}

export function renderOverview(): void {
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
  const pipelineRows =
    model.pipelines
      .map(
        (pipe) => `<tr class="border-base-content/5 hover:bg-base-100/60 border-t">
          <td class="min-w-56 py-3">
            <button type="button" class="group flex max-w-xs text-left js-open-pipeline" data-pipeline-id="${escapeHtml(pipe.id)}">
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
      .join("") ||
    '<tr><td colspan="7" class="text-base-content/70 px-4 py-6">No pipelines configured.</td></tr>';
  const markup = `<div class="space-y-4">
    <header class="flex flex-wrap items-end justify-between gap-3">
      <div>
        <p class="text-accent text-xs font-semibold uppercase tracking-wider">Live operations</p>
        <h1 class="mt-1 text-2xl font-semibold">Fleet overview</h1>
        <p class="text-base-content/70 mt-1 text-sm">See what needs action before scanning throughput and system load.</p>
      </div>
      <button type="button" class="btn btn-sm btn-primary" id="overview-add-pipeline-btn">Add Pipeline</button>
    </header>
    <div class="grid items-start gap-4 lg:grid-cols-[minmax(0,1.2fr)_minmax(22rem,0.8fr)]">
      ${overviewAttentionSection(model)}
      <aside id="overview-fleet-signals" aria-label="Fleet signals" class="border-base-content/10 bg-base-200/80 rounded-lg border p-3">
        <div class="mb-3 flex items-center justify-between gap-3 px-1">
          <h2 class="text-sm font-semibold">Fleet signals</h2>
          <span class="badge badge-outline">${model.counts.pipelines} pipeline${model.counts.pipelines === 1 ? "" : "s"}</span>
        </div>
        <div class="grid grid-cols-2 gap-2">${model.metrics.map(overviewMetric).join("")}</div>
      </aside>
    </div>
    <section id="overview-pipelines" class="dashboard-table-panel">
      <div class="dashboard-section-header">
        <div>
          <h2 class="dashboard-section-title">All pipelines</h2>
          <p class="dashboard-subtitle">Compare intent, runtime state, and data flow.</p>
        </div>
      </div>
      <div class="overflow-x-auto">
        <table class="table table-sm">
          <thead class="text-base-content/70 bg-base-100/50 text-xs uppercase">
            <tr><th>Pipeline</th><th>State</th><th>Input</th><th>Outputs</th><th>Input Rate</th><th>Output Rate</th><th>Recording</th></tr>
          </thead>
          <tbody>${pipelineRows}</tbody>
        </table>
      </div>
    </section>
    <section class="dashboard-section">
      <div class="dashboard-section-header">
        <div>
          <h2 class="dashboard-section-title">Restream Activity</h2>
          <p class="dashboard-subtitle">Recent restream-wide event bursts, grouped for operator-friendly review.</p>
        </div>
        <button type="button" class="btn btn-sm btn-outline" id="overview-open-status-btn">Open Status</button>
      </div>
      <div id="overview-activity-list" class="p-4">${renderRestreamActivityCards(overviewActivityLogs, 6)}</div>
    </section>
  </div>`;
  if (container.innerHTML === markup) return;
  container.innerHTML = markup;
  container.querySelectorAll<HTMLElement>(".js-open-pipeline").forEach((button) => {
    button.onclick = () => {
      if (!button.dataset.pipelineId) return;
      selectPipeline(button.dataset.pipelineId);
      window.setDashboardMode("pipeline");
    };
  });
  container
    .querySelectorAll<HTMLElement>(".js-inspect-pipeline")
    .forEach((button) => {
      button.onclick = () => {
        if (!button.dataset.pipelineId) return;
        selectPipeline(button.dataset.pipelineId);
        window.setDashboardMode("inspect");
      };
    });
  container.querySelector<HTMLElement>("#overview-add-pipeline-btn")?.addEventListener(
    "click",
    () => void window.addPipeBtn(),
  );
  container
    .querySelectorAll<HTMLElement>(
      "#overview-open-status-btn, #overview-open-status-detail-btn",
    )
    .forEach((button) => {
      button.onclick = () => window.setDashboardMode("status");
    });
}

export function syncOverviewActivityStream(): void {
  if (!overviewActivityStreamingEnabled()) {
    closeOverviewActivityStream();
    return;
  }
  refreshOverviewActivityIfStale();
}
