// The span a replay shows, and the moves on it: a preset span ending now,
// zooming out and in around the middle, a span dragged across the overview,
// and the span around an incident. Pure; the page keeps the window in the
// URL (`from`, `to`) so a link reopens the same story.

import { absoluteTime, duration } from "../when.js";

/** `toMs: null` follows now: the right edge is the live moment. */
export interface ReplayWindow {
  readonly fromMs: number;
  readonly toMs: number | null;
}

const MINUTE = 60_000;
export const MIN_SPAN_MS = 2 * MINUTE;

export const PRESETS: ReadonlyArray<readonly [string, number]> = [
  ["10 min", 10 * MINUTE],
  ["1 h", 60 * MINUTE],
  ["6 h", 6 * 60 * MINUTE],
  ["24 h", 24 * 60 * MINUTE],
];

/** The window's edges now. */
export function edges(window: ReplayWindow, nowMs: number): { fromMs: number; toMs: number } {
  return { fromMs: window.fromMs, toMs: window.toMs ?? nowMs };
}

/** A span ending now, following. */
export function lastSpan(spanMs: number, nowMs: number): ReplayWindow {
  return { fromMs: nowMs - spanMs, toMs: null };
}

/** Kept inside the history and at least MIN_SPAN_MS wide. */
export function clampWindow(window: ReplayWindow, oldestMs: number, nowMs: number): ReplayWindow {
  const { fromMs, toMs } = edges(window, nowMs);
  const to = Math.min(toMs, nowMs);
  const from = Math.max(oldestMs, Math.min(fromMs, to - MIN_SPAN_MS));
  return { fromMs: from, toMs: window.toMs === null || to >= nowMs ? null : Math.max(to, from + MIN_SPAN_MS) };
}

/**
 * Zooms by `factor` (2 shows twice the span, 0.5 half) around the middle;
 * a following window keeps its right edge on now.
 */
export function zoom(window: ReplayWindow, factor: number, oldestMs: number, nowMs: number): ReplayWindow {
  const { fromMs, toMs } = edges(window, nowMs);
  const span = Math.max(MIN_SPAN_MS, (toMs - fromMs) * factor);
  if (window.toMs === null) return clampWindow({ fromMs: nowMs - span, toMs: null }, oldestMs, nowMs);
  const middle = (fromMs + toMs) / 2;
  return clampWindow({ fromMs: middle - span / 2, toMs: middle + span / 2 }, oldestMs, nowMs);
}

/** The span between two points dragged across an overview of `[fromMs, toMs]`, as fractions 0..1. */
export function fromDrag(a: number, b: number, overview: { fromMs: number; toMs: number }, nowMs: number): ReplayWindow {
  const [left, right] = a <= b ? [a, b] : [b, a];
  const span = overview.toMs - overview.fromMs;
  const window = { fromMs: overview.fromMs + left * span, toMs: overview.fromMs + right * span };
  // A drag that reaches the live edge keeps following.
  return clampWindow(right >= 0.995 ? { fromMs: window.fromMs, toMs: null } : window, overview.fromMs, nowMs);
}

/** From a little before an incident opened to a little after it closed (or now). */
export function incidentWindow(openedMs: number, closedMs: number | null): ReplayWindow {
  return { fromMs: openedMs - 2 * MINUTE, toMs: closedMs === null ? null : closedMs + 2 * MINUTE };
}

/** How many samples a window asks for: every 2 s sample up to 1,200. */
export function pointsFor(window: ReplayWindow, nowMs: number): number {
  const { fromMs, toMs } = edges(window, nowMs);
  return Math.max(2, Math.min(1_200, Math.ceil((toMs - fromMs) / 2_000)));
}

/** "Today 14:02 BST → now · 40 min". */
export function windowLabel(window: ReplayWindow, nowMs: number, timeZone?: string): string {
  const { fromMs, toMs } = edges(window, nowMs);
  const end = window.toMs === null ? "now" : absoluteTime(toMs, nowMs, { timeZone });
  return `${absoluteTime(fromMs, nowMs, { timeZone })} → ${end} · ${duration(toMs - fromMs)}`;
}
