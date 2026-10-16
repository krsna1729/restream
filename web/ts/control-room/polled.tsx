// Per-view polling for data the live store does not carry (one pipeline's
// inputs, graph or resource map), read through TanStack Query and shown with
// the age of what is on screen.

import { useQuery } from "@tanstack/react-query";
import type { QueryKey } from "@tanstack/react-query";

import { formatAge } from "./format.js";
import { SAMPLE_INTERVAL_MS, SAMPLE_STALE_AFTER_MS, useNow } from "./store.js";

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
