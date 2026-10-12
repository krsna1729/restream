// The control room's one live store: the revisioned runtime state (pushed),
// plus engine telemetry and host metrics (sampled). Views read it through
// `useLive`; nothing else holds a copy.

import { useSyncExternalStore } from "react";

import { getEngineTelemetry, getSystemMetrics } from "../core/api.js";
import type { EngineTelemetrySnapshot } from "../core/api-types.js";
import { openRuntimeStateStream } from "../core/state-stream.js";
import type {
  RuntimeState,
  RuntimeStateStreamStatus,
} from "../core/state-stream.js";
import type { SystemMetrics } from "../types.js";
import { telemetryRates } from "./rates.js";
import type { TelemetryRates } from "./rates.js";

export interface LiveSnapshot {
  readonly state: RuntimeState | null;
  readonly stream: RuntimeStateStreamStatus;
  readonly telemetry: EngineTelemetrySnapshot | null;
  readonly rates: TelemetryRates | null;
  readonly metrics: SystemMetrics | null;
  /** Wall-clock ms of the last successful sample of each sampled source. */
  readonly telemetryAtMs: number | null;
  readonly metricsAtMs: number | null;
}

export const SAMPLE_INTERVAL_MS = 2000;

const EMPTY: LiveSnapshot = {
  state: null,
  stream: "connecting",
  telemetry: null,
  rates: null,
  metrics: null,
  telemetryAtMs: null,
  metricsAtMs: null,
};

let snapshot = EMPTY;
const listeners = new Set<() => void>();
let stop: (() => void) | null = null;

function publish(next: Partial<LiveSnapshot>): void {
  snapshot = { ...snapshot, ...next };
  for (const listener of listeners) listener();
}

async function sample(signal: AbortSignal): Promise<void> {
  const [telemetry, metrics] = await Promise.all([
    getEngineTelemetry(),
    getSystemMetrics({ view: "summary" }),
  ]);
  if (signal.aborted) return;
  const now = Date.now();
  // A failed source keeps its last value; its timestamp shows the age.
  publish({
    ...(telemetry && {
      telemetry,
      rates: telemetryRates(snapshot.telemetry, telemetry) ?? snapshot.rates,
      telemetryAtMs: now,
    }),
    ...(metrics && { metrics, metricsAtMs: now }),
  });
}

// Sources run while at least one view is subscribed. A restart begins from
// nothing: a rate across the gap would be shown as current.
function start(): () => void {
  snapshot = EMPTY;
  const stream = openRuntimeStateStream({
    onState: (state) => publish({ state }),
    onStatus: (status) => publish({ stream: status }),
  });
  const abort = new AbortController();
  let timer: number | undefined;
  const loop = async () => {
    try {
      await sample(abort.signal);
    } catch (error) {
      console.error("control room sample failed", error);
    } finally {
      if (!abort.signal.aborted) {
        timer = window.setTimeout(loop, SAMPLE_INTERVAL_MS);
      }
    }
  };
  void loop();
  return () => {
    abort.abort();
    window.clearTimeout(timer);
    stream.close();
  };
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  stop ??= start();
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && stop) {
      stop();
      stop = null;
    }
  };
}

export function useLive(): LiveSnapshot {
  return useSyncExternalStore(subscribe, () => snapshot);
}
