import { getRestreamHistory } from "../../core/api.js";
import { createManagedLogStream } from "../../core/log-stream.js";
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
      renderOverviewActivity();
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
      renderOverviewActivity();
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
    listEl.innerHTML = renderRestreamActivityCards(
      overviewActivityLogs,
      OVERVIEW_ACTIVITY_LIMIT,
    );
  }
}

export function syncOverviewActivityStream(): void {
  if (!overviewActivityStreamingEnabled()) {
    closeOverviewActivityStream();
    return;
  }
  refreshOverviewActivityIfStale();
}
