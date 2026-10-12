// The Monitor workspace's model: one pipeline's signals sampled on one clock
// and kept for a bounded window in the page (the engine keeps no history),
// so an operator sees which signal moved first.

import type { GraphData, ResourceMapSnapshot } from "../core/api-types.js";
import type { OutputState } from "../core/generated/runtime-state.js";
import { isDelivering } from "./fleet-model.js";
import { ringFromGraph, shardsFrom } from "./inspect-model.js";
import type { TelemetryRates } from "./rates.js";

/** One instant; `null` where the source had nothing to say. */
export interface MonitorSample {
  readonly atMs: number;
  readonly ingestBps: number | null;
  readonly egressBps: number | null;
  /** Wanted outputs delivering, as a percentage; `null` when none is wanted. */
  readonly deliveringPercent: number | null;
  readonly maxFeedLag: number | null;
  readonly ringFillPercent: number | null;
  readonly maxShardProgressMs: number | null;
}

export const SIGNALS = [
  "ingestBps",
  "egressBps",
  "deliveringPercent",
  "maxFeedLag",
  "ringFillPercent",
  "maxShardProgressMs",
] as const;

export type Signal = (typeof SIGNALS)[number];

function max(values: Iterable<number | null>): number | null {
  let best: number | null = null;
  for (const value of values) {
    if (value !== null && (best === null || value > best)) best = value;
  }
  return best;
}

export function takeSample(input: {
  atMs: number;
  pipelineId: string;
  outputs: readonly OutputState[];
  rates: TelemetryRates | null;
  graph: GraphData | null;
  /** The runtime-wide map: shards are process-wide. */
  resources: ResourceMapSnapshot | null;
}): MonitorSample {
  const outputs = input.outputs.filter((o) => o.pipelineId === input.pipelineId);
  const wanted = outputs.filter((o) => o.desired === "running");
  let egressBps: number | null = null;
  for (const output of outputs) {
    const rate = input.rates?.egressBps.get(output.id);
    if (rate !== undefined) egressBps = (egressBps ?? 0) + rate;
  }
  const ring = input.graph ? ringFromGraph(input.graph) : null;
  return {
    atMs: input.atMs,
    ingestBps: input.rates?.ingestBps.get(input.pipelineId) ?? null,
    egressBps,
    deliveringPercent:
      wanted.length === 0
        ? null
        : (100 * wanted.filter(isDelivering).length) / wanted.length,
    maxFeedLag: max(
      (input.graph?.nodes ?? [])
        .filter((node) => node.outputId && node.details)
        .map((node) => {
          const lag = node.details?.feedLagUnits;
          return typeof lag === "number" && Number.isFinite(lag) ? lag : null;
        }),
    ),
    ringFillPercent: ring ? (100 * ring.fill) / ring.capacity : null,
    // A truncated map may have dropped the slowest shard: a gap, not a guess.
    maxShardProgressMs:
      input.resources && !(input.resources.limits?.truncatedNodeCount ?? 0)
        ? max(shardsFrom(input.resources).map((shard) => shard.progressAgeMs))
        : null,
  };
}

/**
 * `history` with `sample` appended, keeping the samples no older than
 * `windowMs` before it. Bounded by time, not count: a hidden tab's timers
 * slow down, and the window must still mean what its label says.
 */
export function appendSample(
  history: readonly MonitorSample[],
  sample: MonitorSample,
  windowMs: number,
): readonly MonitorSample[] {
  return [...history.filter((kept) => kept.atMs >= sample.atMs - windowMs), sample];
}

/**
 * SVG polyline point lists for one signal over `[fromMs, toMs]`, scaled to
 * `width` x `height` with 0 at the bottom and the window's maximum at the
 * top. A missing value breaks the line rather than drawing a zero; a lone
 * value between gaps is a zero-length segment, drawn as a dot by a round cap.
 */
export function sparkline(
  history: readonly MonitorSample[],
  signal: Signal,
  fromMs: number,
  toMs: number,
  width: number,
  height: number,
): readonly string[] {
  const top = max(history.map((sample) => sample[signal])) ?? 0;
  const span = Math.max(1, toMs - fromMs);
  const segments: string[][] = [[]];
  for (const sample of history) {
    const value = sample[signal];
    if (value === null) {
      segments.push([]);
      continue;
    }
    const x = ((sample.atMs - fromMs) / span) * width;
    const y = top > 0 ? height - (value / top) * height : height;
    segments[segments.length - 1]?.push(`${x.toFixed(1)},${y.toFixed(1)}`);
  }
  return segments
    .filter((points) => points.length > 0)
    .map((points) =>
      points.length === 1 ? `${points[0]} ${points[0]}` : points.join(" "),
    );
}
