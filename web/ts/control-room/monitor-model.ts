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
  /** This pipeline's wanted outputs with a diagnosed fault; `null` while the
   * state stream is not live. Its rises from 0 are the incident cursor. */
  readonly degradedOutputs: number | null;
}

export const SIGNALS = [
  "ingestBps",
  "egressBps",
  "deliveringPercent",
  "maxFeedLag",
  "ringFillPercent",
  "maxShardProgressMs",
  "degradedOutputs",
] as const;

export type Signal = (typeof SIGNALS)[number];

export function takeSample(input: {
  atMs: number;
  pipelineId: string;
  /** `null` while the state stream is down: its last snapshot is not now. */
  outputs: readonly OutputState[] | null;
  rates: TelemetryRates | null;
  graph: GraphData | null;
  /** The runtime-wide map: shards are process-wide. */
  resources: ResourceMapSnapshot | null;
}): MonitorSample {
  const outputs = input.outputs?.filter((o) => o.pipelineId === input.pipelineId) ?? null;
  const wanted = outputs?.filter((o) => o.desired === "running") ?? null;
  let egressBps: number | null = null;
  for (const output of outputs ?? []) {
    const rate = input.rates?.egressBps.get(output.id);
    if (rate !== undefined) egressBps = (egressBps ?? 0) + rate;
  }
  const ring = input.graph ? ringFromGraph(input.graph) : null;
  return {
    atMs: input.atMs,
    ingestBps: input.rates?.ingestBps.get(input.pipelineId) ?? null,
    egressBps,
    deliveringPercent:
      wanted === null || wanted.length === 0
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
    degradedOutputs: wanted === null ? null : wanted.filter((o) => o.diagnosis).length,
  };
}

/**
 * When this pipeline's degraded-output count rose from 0: one cursor per
 * incident onset the page saw. A gap (`null`) neither starts nor ends one, and
 * a pipeline already degraded when the page opened has no onset in view.
 */
export function incidentOnsets(history: readonly MonitorSample[]): readonly number[] {
  const onsets: number[] = [];
  let previous: number | null = null;
  for (const sample of history) {
    if (sample.degradedOutputs === null) continue;
    if (previous === 0 && sample.degradedOutputs > 0) onsets.push(sample.atMs);
    previous = sample.degradedOutputs;
  }
  return onsets;
}
