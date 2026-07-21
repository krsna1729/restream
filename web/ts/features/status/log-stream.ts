import { createManagedLogStream } from "../../core/log-stream.js";
import { handleDashboardRuntimeLifecycleLog } from "../dashboard.js";
import { updateRestreamProcessIndicatorFromLog } from "../restream-process-indicator.js";
import type { AppLogRow } from "../../types.js";

let statusLogHistory: AppLogRow[] = [];
let statusLogStreamActive = false;
let currentStatusModeActive = false;
let hasStatusDataSnapshot = false;
let statusUpdateCallback: (() => void) | null = null;
const statusLogStream = createManagedLogStream();

export function setStatusStreamActive(active: boolean): void {
  currentStatusModeActive = active;
  syncStatusStreamVisibility();
}

export function setHasStatusDataSnapshot(hasData: boolean): void {
  hasStatusDataSnapshot = hasData;
}

export function setStatusUpdateCallback(cb: (() => void) | null): void {
  statusUpdateCallback = cb;
}

function latestStatusLogId(): number | null {
  const ids = statusLogHistory
    .map((log) => Number(log?.id))
    .filter((value) => Number.isFinite(value) && value > 0);
  return ids.length > 0 ? Math.max(...ids) : null;
}

export function processStatusLogLine(data: AppLogRow, onLogReceived?: () => void): void {
  statusLogHistory.push(data);
  if (statusLogHistory.length > 500) {
    statusLogHistory.shift();
  }
  updateRestreamProcessIndicatorFromLog(data);
  if (data.eventClass === "lifecycle" || (!data.eventClass && Boolean(data.eventType))) {
    handleDashboardRuntimeLifecycleLog(data);
  }
  onLogReceived?.();
  statusUpdateCallback?.();
}

export function syncStatusStreamVisibility(onLogReceived?: () => void): void {
  const shouldBeActive = !document.hidden && currentStatusModeActive && hasStatusDataSnapshot;
  if (!shouldBeActive) {
    statusLogStream.close();
    statusLogStreamActive = false;
    return;
  }

  statusLogStreamActive = typeof EventSource === "function";
  statusLogStream.sync({
    filters: { scope: "restream" },
    resumeAfterId: latestStatusLogId(),
    onLog: (data) => processStatusLogLine(data, onLogReceived),
  });
}

export function getStatusLogs(): AppLogRow[] {
  return statusLogHistory;
}
