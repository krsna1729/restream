import { getRestreamHistory } from "../../core/api.js";
import { createManagedLogStream } from "../../core/log-stream.js";
import { state } from "../../core/state.js";
import { handleDashboardRuntimeLifecycleLog } from "../../features/dashboard.js";
import {
  buildRestreamActivityBursts,
} from "../../features/overview-activity.js";
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
let overviewPresentationHook:
  ((presentation: OverviewPresentationInput) => void) | null = null;
let lastOverviewMetricsSampleKey: string | null = null;

export function configureOverviewPresentation(options: {
  onPresentation?: (presentation: OverviewPresentationInput) => void;
}): void {
  overviewPresentationHook = options.onPresentation || null;
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

function publishOverviewPresentation(): void {
  overviewPresentationHook?.(currentOverviewPresentation());
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
  publishOverviewPresentation();
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

export function renderOverview(): void {
  refreshOverviewActivityIfStale();

  const counts = buildOverviewViewModel(state.pipelines).counts;
  recordOverviewMetricSamples(counts);
  publishOverviewPresentation();
}

export function syncOverviewActivityStream(): void {
  if (!overviewActivityStreamingEnabled()) {
    closeOverviewActivityStream();
    return;
  }
  refreshOverviewActivityIfStale();
}
