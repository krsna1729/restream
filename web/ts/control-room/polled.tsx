// Per-view polling for data the live store does not carry (a resource map,
// alerts, events, the engine's fleet history), read through TanStack Query
// and shown with the age of what is on screen.

import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { QueryKey } from "@tanstack/react-query";

import {
  getAggregateAlerts,
  getEngineHealth,
  getFleetHistory,
  getLifecycleEvents,
} from "../core/api.js";
import type { AlertsSnapshot, LifecycleEventsSnapshot } from "../core/api-types.js";
import type { FleetHistoryWindow } from "../core/generated/runtime-state.js";
import type { HealthData } from "../types.js";
import { formatAge } from "./format.js";
import { fleetSpanSettled } from "./incidents-model.js";
import { SAMPLE_INTERVAL_MS, SAMPLE_STALE_AFTER_MS, useNow } from "./store.js";

/** How often `/engine/health` reloads: it samples host settings and every
 * pipeline, and most of it changes rarely. */
const HEALTH_INTERVAL_MS = 10_000;

/** How often the fleet history is asked for its new samples: the engine
 * takes one every 5 s. */
const FLEET_HISTORY_INTERVAL_MS = 5_000;

/** The lifecycle events a panel reads at most. */
const EVENT_LIMIT = 200;

export interface Polled<T> {
  readonly value: T | null;
  /** Wall-clock ms of the last successful load. */
  readonly atMs: number | null;
  /** Older than this, the value missed at least two loads. */
  readonly staleAfterMs: number;
  readonly refresh: () => void;
}

/** A load that read nothing: no value to show, not a failure to report. */
export class NoValue extends Error {}

/**
 * `queryKey` names the subject; `load` runs now and every `intervalMs` while
 * the panel is mounted (until `settled`, when given, says a value is final),
 * and `refresh` loads now. A load that reads nothing
 * (`null`) is not a value: the last one stays, and `atMs` shows its age. Two
 * panels naming the same subject share one query.
 */
export function usePolled<T>(
  queryKey: QueryKey,
  load: () => Promise<T | null>,
  intervalMs: number = SAMPLE_INTERVAL_MS,
  settled: ((value: T) => boolean) | null = null,
): Polled<T> {
  const final = (value: T | undefined) =>
    value !== undefined && settled !== null && settled(value);
  const query = useQuery({
    queryKey,
    queryFn: async () => {
      const next = await load();
      if (next === null) throw new NoValue();
      return next;
    },
    refetchInterval: (current) => (final(current.state.data) ? false : intervalMs),
  });
  // The query keeps its data when a load fails, so an empty read leaves the
  // last value and its age in place; a new subject starts with no data.
  const value = query.data ?? null;
  return {
    value,
    atMs: value === null ? null : query.dataUpdatedAt,
    // A settled subject no longer changes, so its age is never stale.
    staleAfterMs:
      final(query.data)
        ? Number.POSITIVE_INFINITY
        : intervalMs * (SAMPLE_STALE_AFTER_MS / SAMPLE_INTERVAL_MS),
    refresh: () => void query.refetch(),
  };
}

/** The engine's health report, shared by every panel that reads it. */
export function useEngineHealth(): Polled<HealthData> {
  return usePolled(["engine-health"], () => getEngineHealth<HealthData>(), HEALTH_INTERVAL_MS);
}

/** Every open alert, shared by Fleet's count and the Incidents list, and
 * the ones resolved in the last `resolvedWindowMs` (all kept without it). */
export function useOpenAlerts(resolvedWindowMs?: number): Polled<AlertsSnapshot> {
  return usePolled(["aggregate-alerts", resolvedWindowMs ?? null], () =>
    getAggregateAlerts(
      resolvedWindowMs === undefined
        ? {}
        : { since: new Date(Date.now() - resolvedWindowMs) },
    ),
  );
}

/** The latest lifecycle events of one pipeline (`null`: of every one). */
export function useLifecycleEvents(
  pipelineId: string | null,
): Polled<LifecycleEventsSnapshot> {
  return usePolled(["lifecycle-events", pipelineId, EVENT_LIMIT], () =>
    getLifecycleEvents({ pipelineId, limit: EVENT_LIMIT }),
  );
}

/**
 * The engine's fleet samples from `fromMs` (every kept one when it is not
 * finite) through `toMs` (`null`: up to now). The first load reads the whole
 * span, each later one only the samples after the last held, so an incident
 * hours long is not re-read every few seconds; held samples the engine no
 * longer keeps are dropped. A closed span stops loading once the server's
 * newest sample (or the start of what it keeps) is at or past its end: every
 * sample in it was taken, by the server's clock, not this browser's.
 */
export function useFleetHistory(
  fromMs: number,
  toMs: number | null,
): Polled<FleetHistoryWindow> {
  const client = useQueryClient();
  const key = ["fleet-history", fromMs, toMs];
  return usePolled(
    key,
    async () => {
      const held = client.getQueryData<FleetHistoryWindow>(key);
      const last = held?.samples[held.samples.length - 1];
      const sinceMs = last ? last.atMs + 1 : fromMs;
      const next = await getFleetHistory({
        since: Number.isFinite(sinceMs) ? new Date(sinceMs) : undefined,
        until: toMs === null ? undefined : new Date(toMs),
      });
      if (next === null) return null;
      const kept = (held?.samples ?? []).filter(
        (sample) => sample.atMs > next.completeFromMs,
      );
      return { ...next, samples: [...kept, ...next.samples] };
    },
    FLEET_HISTORY_INTERVAL_MS,
    toMs === null ? null : (window) => fleetSpanSettled(window, toMs),
  );
}

/** How old a polled panel's data is; amber once it missed two loads. */
export function Freshness({
  atMs,
  staleAfterMs = SAMPLE_STALE_AFTER_MS,
}: {
  atMs: number | null;
  staleAfterMs?: number;
}) {
  const now = useNow(1000);
  if (atMs === null) return null;
  const stale = now - atMs > staleAfterMs;
  return (
    <span
      className={`badge badge-sm ${stale ? "badge-warning" : "badge-ghost"}`}
      title="time since the last successful update"
    >
      {stale ? "stale · " : ""}
      {formatAge(atMs, now)}
    </span>
  );
}
