// The control room's one live store: the revisioned runtime state (pushed),
// plus engine telemetry and host metrics (sampled). Views read it through
// `useLive`; nothing else holds a copy.

import { useEffect, useState, useSyncExternalStore } from "react";

import { getEngineTelemetry, getSystemMetrics } from "../core/api.js";
import type { EngineTelemetrySnapshot } from "../core/api-types.js";
import { openRuntimeStateStream } from "../core/state-stream.js";
import type {
  RuntimeState,
  RuntimeStateStreamStatus,
} from "../core/state-stream.js";
import type { SystemMetrics } from "../types.js";
import { takeFleetSample } from "./fleet-history.js";
import type { FleetSample } from "./fleet-history.js";
import { appendTrends, telemetryRates } from "./rates.js";
import type { TelemetryRates } from "./rates.js";
import { appendSample, WINDOW_MS } from "./sample-window.js";

export interface LiveSnapshot {
  readonly state: RuntimeState | null;
  readonly stream: RuntimeStateStreamStatus;
  readonly telemetry: EngineTelemetrySnapshot | null;
  readonly rates: TelemetryRates | null;
  readonly metrics: SystemMetrics | null;
  /** Wall-clock ms of the last successful sample of each sampled source. */
  readonly telemetryAtMs: number | null;
  readonly metricsAtMs: number | null;
  /** Fleet-wide signals, one per sample, for the last `WINDOW_MS` of this
   * session (empty after a restart). */
  readonly history: readonly FleetSample[];
  /** Each pipeline's ingest bits per second over the last minute, oldest
   * first, for the trend in its row. */
  readonly ingestTrend: ReadonlyMap<string, readonly number[]>;
}

export const SAMPLE_INTERVAL_MS = 2000;

/** A sampled value older than this missed at least two samples. */
export const SAMPLE_STALE_AFTER_MS = SAMPLE_INTERVAL_MS * 2.5;

/** Wall-clock ms, re-rendering every `intervalMs`, for ages on screen. */
export function useNow(intervalMs: number): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), intervalMs);
    return () => window.clearInterval(timer);
  }, [intervalMs]);
  return now;
}

const EMPTY: LiveSnapshot = {
  state: null,
  stream: "connecting",
  telemetry: null,
  rates: null,
  metrics: null,
  telemetryAtMs: null,
  metricsAtMs: null,
  history: [],
  ingestTrend: new Map(),
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
  const sampled = takeFleetSample({
    atMs: now,
    // While the stream is down, its last snapshot is not now.
    outputs: snapshot.stream === "live" ? (snapshot.state?.outputs ?? null) : null,
    capacity: metrics?.capacity ?? null,
  });
  // A failed source keeps its last value; its timestamp shows the age.
  const rates = telemetry ? telemetryRates(snapshot.telemetry, telemetry) : null;
  publish({
    ...(telemetry && {
      telemetry,
      rates: rates ?? snapshot.rates,
      telemetryAtMs: now,
    }),
    ...(rates && { ingestTrend: appendTrends(snapshot.ingestTrend, rates.ingestBps) }),
    ...(metrics && { metrics, metricsAtMs: now }),
    history: appendSample(snapshot.history, sampled, WINDOW_MS),
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
