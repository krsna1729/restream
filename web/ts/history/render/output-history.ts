import { sanitizeLogMessage } from "../../core/utils.js";
import type { AppLogRow } from "../../types.js";
import type { OutputHistoryState, HistoryConstants } from "../state.js";
import {
  formatHistoryTime,
  getCorrelationId,
  getRawHistoryHaystack,
  getOrderedOutputLogs,
  parseHistoryTimeMs,
  renderEventDataSummary,
  renderCorrelationBadge,
} from "./utils.js";
import { classifyHistoryEvent } from "./classify.js";
import { getHistoryRenderCallbacks } from "./callbacks.js";

export function getOutputHistoryContextKey(
  log: AppLogRow | null | undefined,
): string {
  return `${log?.ts || ""}::${log?.message || ""}`;
}

function getRawHistorySearchValue(state: OutputHistoryState): string {
  return String(state.rawQuery || "")
    .trim()
    .toLowerCase();
}

function getFilteredRawOutputLogs(state: OutputHistoryState): AppLogRow[] {
  return getOrderedOutputLogs(state.rawLogs, state.order);
}

export function getMatchingRawOutputLogs(
  state: OutputHistoryState,
): AppLogRow[] {
  const query = getRawHistorySearchValue(state);
  if (!query) return [];
  return getFilteredRawOutputLogs(state).filter((log) => {
    const haystack = getRawHistoryHaystack(log);
    return haystack.includes(query);
  });
}

function getTimelineContextLogs(
  state: OutputHistoryState,
  log: AppLogRow,
): AppLogRow[] {
  return state.contextLogsByKey.get(getOutputHistoryContextKey(log)) || [];
}

export function getTimelineContextRange(
  state: OutputHistoryState,
  constants: HistoryConstants,
  log: AppLogRow,
): { since: string; until: string } | null {
  const targetMs = parseHistoryTimeMs(log?.ts);
  if (targetMs === null) return null;

  const lifecycleLogsAsc = getOrderedOutputLogs(state.lifecycleLogs, "asc");
  const targetIndex = lifecycleLogsAsc.findIndex(
    (entry) =>
      entry?.ts === log?.ts &&
      String(entry?.message || "") === String(log?.message || ""),
  );
  const previousLifecycle =
    targetIndex > 0 ? lifecycleLogsAsc[targetIndex - 1] : null;
  const previousLifecycleMs = parseHistoryTimeMs(previousLifecycle?.ts);
  const lowerBoundMs = Math.max(
    previousLifecycleMs === null
      ? Number.NEGATIVE_INFINITY
      : previousLifecycleMs,
    targetMs - constants.OUTPUT_HISTORY_CONTEXT_WINDOW_MS,
  );
  const sinceMs = Number.isFinite(lowerBoundMs)
    ? lowerBoundMs
    : targetMs - constants.OUTPUT_HISTORY_CONTEXT_WINDOW_MS;

  return {
    since: new Date(sinceMs).toISOString(),
    until: new Date(targetMs).toISOString(),
  };
}

function renderHighlightedLogMessage(
  container: HTMLElement,
  text: string,
  query: string,
): void {
  container.replaceChildren();
  if (!query) {
    container.textContent = text;
    return;
  }

  const source = String(text || "");
  const lowerSource = source.toLowerCase();
  const needle = String(query || "").toLowerCase();
  if (!needle) {
    container.textContent = source;
    return;
  }

  let cursor = 0;
  while (cursor < source.length) {
    const idx = lowerSource.indexOf(needle, cursor);
    if (idx < 0) {
      container.appendChild(document.createTextNode(source.slice(cursor)));
      break;
    }

    if (idx > cursor) {
      container.appendChild(document.createTextNode(source.slice(cursor, idx)));
    }

    const mark = document.createElement("mark");
    mark.className = "rounded bg-amber-400 px-0.5 text-gray-900";
    mark.textContent = source.slice(idx, idx + needle.length);
    container.appendChild(mark);

    cursor = idx + needle.length;
  }
}

export function focusOutputHistoryRawMatch(state: OutputHistoryState): void {
  const list = document.getElementById("output-history-list");
  if (!list) return;
  const target = list.querySelector(
    `[data-raw-match-index="${state.rawMatchIndex}"]`,
  );
  if (!target) return;
  (target as HTMLElement).scrollIntoView({ block: "nearest" });
}

interface RenderOutputHistoryOptions {
  scrollToTop?: boolean;
  anchorContextKey?: string | null;
}

export function renderOutputHistory(
  state: OutputHistoryState,
  constants: HistoryConstants,
  {
    scrollToTop = false,
    anchorContextKey = null,
  }: RenderOutputHistoryOptions = {},
): void {
  const list = document.getElementById("output-history-list");
  const empty = document.getElementById("output-history-empty");
  const searchWrap = document.getElementById("output-history-search-wrap");
  const searchInput = document.getElementById(
    "output-history-search",
  ) as HTMLInputElement | null;
  const searchStatus = document.getElementById("output-history-search-status");
  const searchPrevBtn = document.getElementById(
    "output-history-search-prev",
  ) as HTMLButtonElement | null;
  const searchNextBtn = document.getElementById(
    "output-history-search-next",
  ) as HTMLButtonElement | null;
  const timelineBtn = document.getElementById("output-history-mode-timeline");
  const rawBtn = document.getElementById("output-history-mode-raw");
  const newestBtn = document.getElementById("output-history-order-newest");
  const oldestBtn = document.getElementById("output-history-order-oldest");

  if (
    !list ||
    !empty ||
    !timelineBtn ||
    !rawBtn ||
    !newestBtn ||
    !oldestBtn ||
    !searchWrap ||
    !searchInput ||
    !searchStatus ||
    !searchPrevBtn ||
    !searchNextBtn
  )
    return;

  const mode = state.mode;
  timelineBtn.classList.toggle("btn-accent", mode === "timeline");
  timelineBtn.classList.toggle("btn-outline", mode !== "timeline");
  rawBtn.classList.toggle("btn-accent", mode === "raw");
  rawBtn.classList.toggle("btn-outline", mode !== "raw");

  const newestFirst = state.order === "desc";
  newestBtn.classList.toggle("btn-accent", newestFirst);
  newestBtn.classList.toggle("btn-outline", !newestFirst);
  oldestBtn.classList.toggle("btn-accent", !newestFirst);
  oldestBtn.classList.toggle("btn-outline", newestFirst);

  searchWrap.classList.toggle("hidden", mode !== "raw");
  if (searchInput.value !== state.rawQuery) {
    searchInput.value = state.rawQuery;
  }

  const rawMatchingLogs = mode === "raw" ? getMatchingRawOutputLogs(state) : [];
  const hasSearchQuery = getRawHistorySearchValue(state).length > 0;
  if (mode === "raw" && hasSearchQuery && rawMatchingLogs.length > 0) {
    if (
      state.rawMatchIndex < 0 ||
      state.rawMatchIndex >= rawMatchingLogs.length
    ) {
      state.rawMatchIndex = 0;
    }
    searchStatus.textContent = `${state.rawMatchIndex + 1}/${rawMatchingLogs.length}`;
  } else if (mode === "raw" && hasSearchQuery) {
    searchStatus.textContent = "0/0";
  } else {
    searchStatus.textContent = "";
  }

  const canNavigateMatches =
    mode === "raw" && hasSearchQuery && rawMatchingLogs.length > 0;
  searchPrevBtn.disabled = !canNavigateMatches;
  searchNextBtn.disabled = !canNavigateMatches;

  list.replaceChildren();

  const hasLogs =
    mode === "raw"
      ? Array.isArray(state.rawLogs) && state.rawLogs.length > 0
      : Array.isArray(state.lifecycleLogs) && state.lifecycleLogs.length > 0;

  if (!hasLogs) {
    empty.classList.remove("hidden");
    return;
  }

  empty.classList.add("hidden");

  if (mode === "raw") {
    const rawLogs = getFilteredRawOutputLogs(state);
    const query = getRawHistorySearchValue(state);
    let matchCounter = 0;
    list.innerHTML = rawLogs
      .map((log) => {
        const haystack = getRawHistoryHaystack(log);
        const isMatch = hasSearchQuery && haystack.includes(query);
        const matchIndex = isMatch ? matchCounter++ : -1;
        const focused = isMatch && matchIndex === state.rawMatchIndex;
        const correlationId = getCorrelationId(log);
        return `<div class="rounded border ${focused ? "border-success" : "border-transparent"} bg-base-100 p-2"
                              ${isMatch ? `data-raw-match-index="${matchIndex}"` : ""}>
                    <div class="flex items-center justify-between gap-2">
                        <div class="flex flex-wrap items-center gap-2">
                            <span class="badge badge-sm badge-ghost">Log</span>
                            ${correlationId ? renderCorrelationBadge(correlationId) : ""}
                        </div>
                        <span class="text-xs opacity-70">${formatHistoryTime(log.ts)}</span>
                    </div>
                    <pre class="mt-1 text-xs whitespace-pre-wrap break-words js-raw-msg"></pre>
                </div>`;
      })
      .join("");
    list.querySelectorAll<HTMLPreElement>(".js-raw-msg").forEach((pre, i) => {
      renderHighlightedLogMessage(
        pre,
        sanitizeLogMessage(rawLogs[i].message || "", false),
        hasSearchQuery ? query : "",
      );
    });
    if (scrollToTop) list.scrollTop = 0;
    return;
  }

  const timelineLogs = getOrderedOutputLogs(state.lifecycleLogs, state.order);
  const callbacks = getHistoryRenderCallbacks();
  timelineLogs.forEach((log, index) => {
    const event = classifyHistoryEvent(log, timelineLogs, index);
    const correlationId = getCorrelationId(log);
    const contextLogs = getTimelineContextLogs(state, log);
    const contextKey = getOutputHistoryContextKey(log);
    const expanded = state.expandedContextKeys.has(contextKey);
    const contextLoading = state.contextLoadingKeys.has(contextKey);
    const orderedContextLogs =
      expanded && !contextLoading && contextLogs.length > 0
        ? getOrderedOutputLogs(contextLogs, state.order)
        : [];

    let contextBoxHtml = "";
    if (expanded) {
      let contextBodyHtml: string;
      if (contextLoading) {
        contextBodyHtml =
          '<div class="text-xs opacity-70">Loading context...</div>';
      } else if (contextLogs.length === 0) {
        contextBodyHtml =
          '<div class="text-xs opacity-70">No stderr, exit, or control logs in the bounded window before this event.</div>';
      } else {
        contextBodyHtml = orderedContextLogs
          .map(
            (cl, i) => `<div class="mb-2 last:mb-0">
                        <div class="text-[11px] opacity-60">${formatHistoryTime(cl.ts)}</div>
                        <pre class="mt-1 text-xs whitespace-pre-wrap break-words js-ctx-msg" data-ctx-i="${i}"></pre>
                    </div>`,
          )
          .join("");
      }
      contextBoxHtml = `<div class="mt-2 rounded border border-base-300 bg-base-200 p-2">
                <div class="mb-2 text-xs font-medium opacity-70">stderr / exit / control before event (${contextLoading ? "…" : contextLogs.length})</div>
                ${contextBodyHtml}
            </div>`;
    }

    const row = document.createElement("div");
    row.className = "rounded bg-base-100 p-2";
    if (contextKey) row.dataset.contextKey = contextKey;
    row.innerHTML = `
            <div class="flex items-center justify-between gap-2">
                <div class="flex items-center gap-2">
                    <button type="button" class="btn btn-ghost btn-xs btn-square text-lg leading-none js-toggle"
                            title="${expanded ? "Hide context" : "Show context"}"
                            aria-label="${expanded ? "Hide context" : "Show context"}"
                            ${contextLoading ? "disabled" : ""}>
                        ${contextLoading ? "…" : expanded ? "▾" : "▸"}
                    </button>
                    <span class="badge badge-sm ${event.badgeClass}">${event.label}</span>
                    ${correlationId ? renderCorrelationBadge(correlationId) : ""}
                </div>
                <span class="text-xs opacity-70">${formatHistoryTime(log.ts)}</span>
            </div>
            <pre class="mt-1 text-xs whitespace-pre-wrap break-words js-log-msg"></pre>
            ${renderEventDataSummary(log)}
            ${contextBoxHtml}
        `;
    (row.querySelector(".js-log-msg") as HTMLPreElement).textContent =
      sanitizeLogMessage(log.message || "", false);
    (row.querySelector(".js-toggle") as HTMLButtonElement).onclick = () =>
      callbacks.toggleOutputHistoryContext?.(log);
    row.querySelectorAll<HTMLPreElement>(".js-ctx-msg").forEach((pre) => {
      pre.textContent = sanitizeLogMessage(
        orderedContextLogs[Number(pre.dataset.ctxI)]?.message || "",
        false,
      );
    });
    list.appendChild(row);
  });

  if (anchorContextKey) {
    const target = list.querySelector(
      `[data-context-key="${CSS.escape(anchorContextKey)}"]`,
    );
    if (target) (target as HTMLElement).scrollIntoView({ block: "nearest" });
  } else if (scrollToTop) {
    list.scrollTop = 0;
  }
}
