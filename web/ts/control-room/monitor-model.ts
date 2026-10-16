// The Monitor workspace's model: one pipeline's signals sampled on one clock
// and kept for a bounded window in the page (the engine keeps no history),
// so an operator sees which signal moved first.

import type { GraphData, ResourceMapSnapshot } from "../core/api-types.js";
import type { OutputState } from "../core/generated/runtime-state.js";
import { isDelivering } from "./fleet-model.js";
import { ringFromGraph, shardsFrom } from "./inspect-model.js";
import type { TelemetryRates } from "./rates.js";
import { max } from "./sample-window.js";

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
