// Client of the revisioned runtime state (Q-042): one snapshot, then deltas
// applied in revision order. A gap (a delta that does not follow the held
// revision) means events were missed; the client reconnects from its last
// revision, and the server answers with the missing deltas or a snapshot.

import { buildRuntimeStateStreamUrl } from "./api.js";
import type {
  OutputState,
  PipelineState,
  RuntimeState,
  StateDelta,
} from "./generated/runtime-state.js";

export type { RuntimeState, StateDelta };

function upsertById<T extends { id: string }>(
  current: T[],
  upserts: T[],
  removed: string[],
): T[] {
  if (upserts.length === 0 && removed.length === 0) return current;
  const byId = new Map(current.map((entity) => [entity.id, entity]));
  for (const id of removed) byId.delete(id);
  for (const entity of upserts) byId.set(entity.id, entity);
  return [...byId.values()].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

/**
 * The state after `delta`, or `null` when `delta` does not directly follow
 * `state` (missed events: resync). An already-applied delta is ignored.
 */
export function applyStateDelta(
  state: RuntimeState,
  delta: StateDelta,
): RuntimeState | null {
  if (delta.revision <= state.revision) return state;
  if (delta.revision !== state.revision + 1) return null;
  return {
    revision: delta.revision,
    pipelines: upsertById<PipelineState>(
      state.pipelines,
      delta.pipelines,
      delta.removedPipelines,
    ),
    outputs: upsertById<OutputState>(
      state.outputs,
      delta.outputs,
      delta.removedOutputs,
    ),
  };
}

type EventSourceLike = {
  addEventListener(type: string, handler: (event: Event) => void): void;
  close(): void;
  onerror: ((event: Event) => void) | null;
};

export type RuntimeStateStreamOptions = {
  onState: (state: RuntimeState) => void;
  /** Injected in tests; defaults to the browser `EventSource`. */
  createEventSource?: (url: string) => EventSourceLike;
  /** Delay before reconnecting after a connection error (default 1000). */
  reconnectDelayMs?: number;
};

export type RuntimeStateStream = {
  current(): RuntimeState | null;
  close(): void;
};

/**
 * Follows `/api/v1/state/stream`. On a gap or a dropped connection it
 * reconnects with `since` = the last applied revision.
 */
export function openRuntimeStateStream(
  options: RuntimeStateStreamOptions,
): RuntimeStateStream {
  const create =
    options.createEventSource ??
    ((url: string) => new EventSource(url) as unknown as EventSourceLike);
  let state: RuntimeState | null = null;
  let source: EventSourceLike | null = null;
  let closed = false;
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  const reconnectDelayMs = options.reconnectDelayMs ?? 1000;

  const connect = () => {
    if (closed) return;
    const opened = create(buildRuntimeStateStreamUrl(state?.revision));
    source = opened;
    const reconnect = (delayMs: number) => {
      if (source !== opened) return;
      opened.close();
      source = null;
      if (delayMs === 0) {
        connect();
      } else {
        retryTimer = setTimeout(() => {
          retryTimer = null;
          connect();
        }, delayMs);
      }
    };
    // Missed deltas: ask for them (or a snapshot) right away.
    const resync = () => reconnect(0);
    opened.addEventListener("snapshot", (event) => {
      if (source !== opened) return;
      state = JSON.parse((event as MessageEvent).data) as RuntimeState;
      options.onState(state);
    });
    opened.addEventListener("delta", (event) => {
      if (source !== opened) return;
      const delta = JSON.parse((event as MessageEvent).data) as StateDelta;
      if (state === null) {
        resync();
        return;
      }
      const next = applyStateDelta(state, delta);
      if (next === null) {
        resync();
        return;
      }
      if (next !== state) {
        state = next;
        options.onState(state);
      }
    });
    // The server is unreachable or restarting: back off, then resume.
    opened.onerror = () => reconnect(reconnectDelayMs);
  };

  connect();
  return {
    current: () => state,
    close: () => {
      closed = true;
      if (retryTimer !== null) clearTimeout(retryTimer);
      source?.close();
      source = null;
    },
  };
}
