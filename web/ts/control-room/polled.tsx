// Per-view polling for data the live store does not carry (one pipeline's
// inputs, graph or resource map), with the age of what is on screen.

import { useEffect, useState } from "react";

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
 * `load` now and every `intervalMs` while mounted; `refresh` loads now.
 * A failed load keeps the last value; `atMs` shows its age. The caller is
 * keyed by its subject, so a new subject remounts and starts from nothing:
 * one pipeline's data never shows under another's name.
 */
export function usePolled<T>(
  load: () => Promise<T | null>,
  intervalMs: number = SAMPLE_INTERVAL_MS,
): Polled<T> {
  const [loaded, setLoaded] = useState<{ value: T; atMs: number } | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    const loop = async () => {
      try {
        const next = await load();
        if (alive && next !== null) setLoaded({ value: next, atMs: Date.now() });
      } catch (error) {
        console.error("control room poll failed", error);
      } finally {
        if (alive) timer = window.setTimeout(loop, intervalMs);
      }
    };
    void loop();
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [tick]);
  return {
    value: loaded?.value ?? null,
    atMs: loaded?.atMs ?? null,
    staleAfterMs: intervalMs * (SAMPLE_STALE_AFTER_MS / SAMPLE_INTERVAL_MS),
    refresh: () => setTick((n) => n + 1),
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
