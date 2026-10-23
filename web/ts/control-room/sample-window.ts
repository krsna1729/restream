// A bounded window of samples kept in the page (the engine keeps no metric
// history), and the sparkline that draws one signal of it. Monitor keeps
// one per pipeline; the live store keeps the fleet-wide one.

/** The window every sampled history keeps: since the page opened, up to
 * this long. */
export const WINDOW_MS = 15 * 60 * 1000;

export { max, sparkline } from "../ui/series-geometry.js";

/**
 * `history` with `sample` appended, keeping the samples no older than
 * `windowMs` before it. Bounded by time, not count: a hidden tab's timers
 * slow down, and the window must still mean what its label says.
 */
export function appendSample<T extends { readonly atMs: number }>(
  history: readonly T[],
  sample: T,
  windowMs: number,
): readonly T[] {
  return [...history.filter((kept) => kept.atMs >= sample.atMs - windowMs), sample];
}
