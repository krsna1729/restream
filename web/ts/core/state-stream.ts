// Client of the revisioned runtime state: one snapshot, then deltas
// applied in revision order. A gap (a delta that does not follow the held
// revision) means events were missed; the client reconnects from its last
// revision, and the server answers with the missing deltas or a snapshot.

import { buildRuntimeStateStreamUrl } from "./api.js";
import type {
  InputState,
  OutputState,
  PipelineGraph,
  PipelineState,
  RuntimeState,
  StateDelta,
} from "./generated/runtime-state.js";

export type { RuntimeState, StateDelta };

/** `current` with `removed` keys gone and `upserts` in place, by key. */
function upsertBy<T>(
  current: T[],
  upserts: T[],
  removed: string[],
  key: (entity: T) => string,
): T[] {
  if (upserts.length === 0 && removed.length === 0) return current;
  const byKey = new Map(current.map((entity) => [key(entity), entity]));
  for (const id of removed) byKey.delete(id);
  for (const entity of upserts) byKey.set(key(entity), entity);
  return [...byKey.entries()]
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([, entity]) => entity);
}

const byId = (entity: { id: string }) => entity.id;

/**
 * The state after `delta`, or `null` when `delta` does not directly follow
 * `state` (missed events: resync). An already-applied delta is ignored.
 */
export function applyStateDelta(
  state: RuntimeState,
  delta: StateDelta,
): RuntimeState | null {
  // Another server run: its revisions do not continue this state's.
  if (delta.epoch !== state.epoch) return null;
  if (delta.revision <= state.revision) return state;
  if (delta.revision !== state.revision + 1) return null;
  return {
    epoch: state.epoch,
    revision: delta.revision,
    pipelines: upsertBy<PipelineState>(
      state.pipelines,
      delta.pipelines,
      delta.removedPipelines,
      byId,
    ),
    outputs: upsertBy<OutputState>(state.outputs, delta.outputs, delta.removedOutputs, byId),
    inputs: upsertBy<InputState>(state.inputs, delta.inputs, delta.removedInputs, byId),
    graphs: upsertBy<PipelineGraph>(
      state.graphs,
      delta.graphs,
      delta.removedGraphs,
      (graph) => graph.pipelineId,
    ),
  };
}

type EventSourceLike = {
  addEventListener(type: string, handler: (event: Event) => void): void;
  close(): void;
  onerror: ((event: Event) => void) | null;
};

/** Whether the stream is connected; state changes alone cannot tell, as a
 * steady system sends no deltas. */
export type RuntimeStateStreamStatus = "connecting" | "live" | "reconnecting";

export type RuntimeStateStreamOptions = {
  onState: (state: RuntimeState) => void;
  onStatus?: (status: RuntimeStateStreamStatus) => void;
  /** Injected in tests; defaults to the browser `EventSource`. */
  createEventSource?: (url: string) => EventSourceLike;
  /** Delay before reconnecting after a connection error (default 1000). */
  reconnectDelayMs?: number;
  /** With no event for this long the connection counts as dead and is
   * replaced (default 50 000: two and a half server heartbeats). */
  silenceTimeoutMs?: number;
};

export type RuntimeStateStream = {
  current(): RuntimeState | null;
  close(): void;
};

/**
 * Follows the runtime state stream (`buildRuntimeStateStreamUrl`). On a gap or a dropped connection it
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
  const silenceTimeoutMs = options.silenceTimeoutMs ?? 50_000;
  let silenceTimer: ReturnType<typeof setTimeout> | undefined;
  let status: RuntimeStateStreamStatus = "connecting";
  const setStatus = (next: RuntimeStateStreamStatus) => {
    if (next === status) return;
    status = next;
    options.onStatus?.(next);
  };

  const connect = () => {
    if (closed) return;
    const opened = create(
      buildRuntimeStateStreamUrl(
        state ? `${state.epoch}.${state.revision}` : undefined,
      ),
    );
    source = opened;
    const reconnect = (delayMs: number) => {
      if (source !== opened) return;
      clearTimeout(silenceTimer);
      opened.close();
      source = null;
      setStatus("reconnecting");
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
    // Any event proves the connection alive; a half-open one raises no
    // error, so silence is the only sign.
    const heard = () => {
      clearTimeout(silenceTimer);
      silenceTimer = setTimeout(() => reconnect(0), silenceTimeoutMs);
    };
    heard();
    opened.addEventListener("open", () => {
      if (source !== opened) return;
      heard();
      setStatus("live");
    });
    opened.addEventListener("heartbeat", () => {
      if (source === opened) heard();
    });
    opened.addEventListener("snapshot", (event) => {
      if (source !== opened) return;
      heard();
      state = JSON.parse((event as MessageEvent).data) as RuntimeState;
      options.onState(state);
    });
    opened.addEventListener("delta", (event) => {
      if (source !== opened) return;
      heard();
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
      clearTimeout(silenceTimer);
      source?.close();
      source = null;
    },
  };
}
