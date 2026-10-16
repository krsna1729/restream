// Per-view polling for data the live store does not carry (one pipeline's
// inputs, graph or resource map), read through TanStack Query and shown with
// the age of what is on screen.

import { useQuery } from "@tanstack/react-query";
import type { QueryKey } from "@tanstack/react-query";
import { useRef } from "react";

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
    queryFn: load,
    refetchInterval: intervalMs,
  });
  // A null load keeps the last value and its age; a new subject starts from
  // nothing, so one pipeline's data never shows under another's name.
  const key = JSON.stringify(queryKey);
  const kept = useRef<{ key: string; value: T; atMs: number } | null>(null);
  if (query.data !== null && query.data !== undefined) {
    kept.current = { key, value: query.data, atMs: query.dataUpdatedAt };
  } else if (kept.current !== null && kept.current.key !== key) {
    kept.current = null;
  }
  const value = kept.current?.value ?? null;
  return {
    value,
    atMs: value === null ? null : (kept.current?.atMs ?? null),
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
