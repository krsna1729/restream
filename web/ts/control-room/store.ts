// The control room's one live store: the revisioned runtime state (pushed),
// plus engine telemetry and host metrics (sampled). Views read it through
// `useLive`; nothing else holds a copy.

import { useSyncExternalStore } from "react";

import { getEngineTelemetry, getSystemMetrics } from "../core/api.js";
import type { EngineTelemetrySnapshot } from "../core/api-types.js";
import { openRuntimeStateStream } from "../core/state-stream.js";
import type { RuntimeState } from "../core/state-stream.js";
import type { SystemMetrics } from "../types.js";
import { telemetryRates } from "./rates.js";
import type { TelemetryRates } from "./rates.js";

export interface LiveSnapshot {
  readonly state: RuntimeState | null;
  readonly telemetry: EngineTelemetrySnapshot | null;
  readonly rates: TelemetryRates | null;
  readonly metrics: SystemMetrics | null;
  /** Wall-clock ms of the last successful sample of each source. */
  readonly stateAtMs: number | null;
  readonly sampledAtMs: number | null;
}

const SAMPLE_INTERVAL_MS = 2000;

let snapshot: LiveSnapshot = {
  state: null,
  telemetry: null,
  rates: null,
  metrics: null,
  stateAtMs: null,
  sampledAtMs: null,
};
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
  publish({
    telemetry: telemetry ?? snapshot.telemetry,
    // A failed sample keeps the last rates: the timestamp shows their age.
    rates: telemetry
      ? (telemetryRates(snapshot.telemetry, telemetry) ?? snapshot.rates)
      : snapshot.rates,
    metrics: metrics ?? snapshot.metrics,
    sampledAtMs: telemetry || metrics ? Date.now() : snapshot.sampledAtMs,
  });
}

// Sources run while at least one view is subscribed.
function start(): () => void {
  const stream = openRuntimeStateStream({
    onState: (state) => publish({ state, stateAtMs: Date.now() }),
  });
  const abort = new AbortController();
  let timer: number | undefined;
  const loop = async () => {
    await sample(abort.signal);
    if (!abort.signal.aborted) {
      timer = window.setTimeout(loop, SAMPLE_INTERVAL_MS);
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
