import type { AppLogRow } from "../../types.js";

export interface HistoryRenderCallbacks {
  toggleOutputHistoryContext: ((log: AppLogRow) => void) | null;
}

const historyRenderCallbacks: HistoryRenderCallbacks = {
  toggleOutputHistoryContext: null,
};

export function getHistoryRenderCallbacks(): HistoryRenderCallbacks {
  return historyRenderCallbacks;
}

export function setHistoryRenderCallbacks(
  callbacks: Partial<HistoryRenderCallbacks>,
): void {
  Object.assign(historyRenderCallbacks, callbacks || {});
}
