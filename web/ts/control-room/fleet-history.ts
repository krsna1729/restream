// The fleet-wide signals the live store samples on its 2 s clock and keeps
// for `WINDOW_MS` (the engine keeps no history): headroom, delivery, and the
// diagnosed outputs by layer. Per-pipeline signals stay in Monitor.

import type {
  CapacitySnapshot,
  Layer,
  OutputState,
} from "../core/generated/runtime-state.js";
import { isDelivering } from "./fleet-model.js";

export const LAYERS: readonly Layer[] = ["ingest", "feed", "shard", "destination"];

/** One instant; `null` where the source had nothing to say. */
export interface FleetSample {
  readonly atMs: number;
  /** 100 less the projected utilization of the hottest service center
   * (calibration priors), floored at 0. */
  readonly headroomPercent: number | null;
  /** Wanted outputs delivering; `null` when none is wanted. */
  readonly deliveringPercent: number | null;
  /** Wanted outputs with a diagnosed fault, by the layer the fault is in. */
  readonly degraded: Readonly<Record<Layer, number>> | null;
}

/**
 * `outputs` is `null` while the state stream is not live (its last snapshot
 * is not now); `capacity` is `null` when this round's metrics read failed.
 */
export function takeFleetSample(input: {
  atMs: number;
  outputs: readonly OutputState[] | null;
  capacity: CapacitySnapshot | null;
}): FleetSample {
  const wanted = input.outputs?.filter((output) => output.desired === "running") ?? null;
  let degraded: Record<Layer, number> | null = null;
  if (wanted !== null) {
    degraded = { ingest: 0, feed: 0, shard: 0, destination: 0 };
    for (const output of wanted) {
      if (output.diagnosis) degraded[output.diagnosis.layer] += 1;
    }
  }
  return {
    atMs: input.atMs,
    headroomPercent: input.capacity
      ? Math.max(0, 100 * (1 - input.capacity.projectedUtilization))
      : null,
    deliveringPercent:
      wanted === null || wanted.length === 0
        ? null
        : (100 * wanted.filter(isDelivering).length) / wanted.length,
    degraded,
  };
}

/** Every diagnosed wanted output in a sample; `null` when it had no state. */
export function degradedTotal(sample: FleetSample): number | null {
  return sample.degraded === null
    ? null
    : LAYERS.reduce((sum, layer) => sum + (sample.degraded?.[layer] ?? 0), 0);
}
