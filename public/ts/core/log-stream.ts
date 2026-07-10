import { buildLogsStreamUrl, type BuildLogsStreamUrlOptions } from "./api.js";
import type { AppLogRow } from "../types.js";

export type LogStreamFilters = Omit<BuildLogsStreamUrlOptions, "lastEventId">;

export interface LogStreamSpec {
  filters?: LogStreamFilters;
  resumeAfterId?: number | null;
  onLog: (log: AppLogRow) => void;
  onUnavailable?: () => void;
}

export interface ManagedLogStream {
  sync(spec: LogStreamSpec | null): void;
  close(): void;
  getLastEventId(): number | null;
}

function positiveEventId(value: unknown): number | null {
  const id = Number(value);
  return Number.isFinite(id) && id > 0 ? id : null;
}

function canonicalFilterUrl(filters: LogStreamFilters): string {
  return buildLogsStreamUrl(filters);
}

export function createManagedLogStream(): ManagedLogStream {
  let source: EventSource | null = null;
  let currentFilterUrl: string | null = null;
  let currentSpec: LogStreamSpec | null = null;
  let lastEventId: number | null = null;
  let generation = 0;
  let unavailableNotified = false;

  function closeSource(): void {
    generation += 1;
    source?.close();
    source = null;
  }

  function notifyUnavailable(): void {
    if (unavailableNotified) return;
    unavailableNotified = true;
    currentSpec?.onUnavailable?.();
  }

  function openSource(): void {
    if (!currentSpec || source) return;
    if (typeof EventSource !== "function") {
      notifyUnavailable();
      return;
    }

    unavailableNotified = false;
    const openedGeneration = generation;
    try {
      const openedSource = new EventSource(
        buildLogsStreamUrl({
          ...(currentSpec.filters || {}),
          lastEventId,
        }),
      );
      source = openedSource;
      openedSource.addEventListener("log", (event: Event) => {
        if (
          source !== openedSource ||
          generation !== openedGeneration ||
          !currentSpec
        ) {
          return;
        }
        try {
          const log = JSON.parse((event as MessageEvent).data) as AppLogRow;
          const eventId = positiveEventId(log.id);
          if (
            eventId !== null &&
            lastEventId !== null &&
            eventId <= lastEventId
          ) {
            return;
          }
          if (eventId !== null) lastEventId = eventId;
          currentSpec.onLog(log);
        } catch {
          // Ignore malformed frames; persisted replay or a snapshot heals state.
        }
      });
      openedSource.onerror = () => {
        if (
          source !== openedSource ||
          generation !== openedGeneration ||
          !currentSpec
        ) {
          return;
        }
        const closedState =
          typeof EventSource.CLOSED === "number" ? EventSource.CLOSED : 2;
        if (openedSource.readyState !== closedState) {
          // Native EventSource reconnect carries Last-Event-ID.
          return;
        }
        closeSource();
        notifyUnavailable();
      };
    } catch {
      closeSource();
      notifyUnavailable();
    }
  }

  return {
    sync(spec: LogStreamSpec | null): void {
      if (!spec) {
        currentSpec = null;
        currentFilterUrl = null;
        closeSource();
        return;
      }

      const nextFilterUrl = canonicalFilterUrl(spec.filters || {});
      const filterChanged = currentFilterUrl !== nextFilterUrl;
      currentSpec = spec;
      if (filterChanged) {
        currentFilterUrl = nextFilterUrl;
        lastEventId = positiveEventId(spec.resumeAfterId);
        closeSource();
      } else {
        const requestedCursor = positiveEventId(spec.resumeAfterId);
        if (
          requestedCursor !== null &&
          (lastEventId === null || requestedCursor > lastEventId)
        ) {
          lastEventId = requestedCursor;
        }
      }
      openSource();
    },
    close(): void {
      currentSpec = null;
      currentFilterUrl = null;
      closeSource();
    },
    getLastEventId(): number | null {
      return lastEventId;
    },
  };
}
