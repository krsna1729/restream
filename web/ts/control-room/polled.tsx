// Per-view polling for data the live store does not carry (one pipeline's
// inputs, graph or resource map), read through TanStack Query and shown with
// the age of what is on screen.

import { useQuery } from "@tanstack/react-query";
import type { QueryKey } from "@tanstack/react-query";

import { getAggregateAlerts, getEngineHealth, getLifecycleEvents } from "../core/api.js";
import type { AlertsSnapshot, LifecycleEventsSnapshot } from "../core/api-types.js";
import type { HealthData } from "../types.js";
import { formatAge } from "./format.js";
import { SAMPLE_INTERVAL_MS, SAMPLE_STALE_AFTER_MS, useNow } from "./store.js";

/** How often `/engine/health` reloads: it samples host settings and every
 * pipeline, and most of it changes rarely. */
const HEALTH_INTERVAL_MS = 10_000;

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
 * the panel is mounted, and `refresh` loads now. A load that reads nothing
 * (`null`) is not a value: the last one stays, and `atMs` shows its age. Two
 * panels naming the same subject share one query.
 */
export function usePolled<T>(
  queryKey: QueryKey,
  load: () => Promise<T | null>,
  intervalMs: number = SAMPLE_INTERVAL_MS,
): Polled<T> {
  const query = useQuery({
    queryKey,
    queryFn: async () => {
      const next = await load();
      if (next === null) throw new NoValue();
      return next;
    },
    refetchInterval: intervalMs,
  });
  // The query keeps its data when a load fails, so an empty read leaves the
  // last value and its age in place; a new subject starts with no data.
  const value = query.data ?? null;
  return {
    value,
    atMs: value === null ? null : query.dataUpdatedAt,
    staleAfterMs: intervalMs * (SAMPLE_STALE_AFTER_MS / SAMPLE_INTERVAL_MS),
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
