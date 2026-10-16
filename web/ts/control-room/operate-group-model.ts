// One egress group of the Operate graph, inspected: how much of it delivers,
// which fabric shards carry it and how they are, and why it is degraded.

import type { TelemetryEgress } from "../core/api-types.js";
import type { Layer, OutputState } from "../core/generated/runtime-state.js";
import type { HealthData } from "../types.js";
import { describeCause, isDelivering } from "./fleet-model.js";
import type { EgressGroup } from "./operate-model.js";

type ShardStatus = NonNullable<HealthData["egressFabricShards"]>[number];

export interface GroupShard {
  readonly feedId: string;
  readonly shardIndex: number;
  /** `null` while the engine health has not reported the shard. */
  readonly status: ShardStatus | null;
}

export interface DegradedOutput {
  readonly name: string;
  readonly layer: Layer;
  readonly cause: string;
  readonly ruledOut: readonly Layer[];
}

export interface GroupInspection {
  readonly outputs: number;
  readonly delivering: number;
  /** Over the running outputs that report one; `null` when none does. */
  readonly minDeliveryRatio: number | null;
  readonly worstFeedLag: number | null;
  readonly resyncs: number;
  /** The fabric shards the running outputs are placed on, by feed. */
  readonly shards: readonly GroupShard[];
  readonly degraded: readonly DegradedOutput[];
}

function finite(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

export function inspectGroup(
  group: EgressGroup,
  outputs: readonly OutputState[],
  egresses: readonly TelemetryEgress[] | null,
  shardStatuses: readonly ShardStatus[] | null,
): GroupInspection {
  const members = new Set(group.outputIds);
  const running = new Map(
    (egresses ?? [])
      .filter((egress) => members.has(egress.outputId))
      .map((egress) => [egress.outputId, egress]),
  );
  let delivering = 0;
  let minDeliveryRatio: number | null = null;
  let worstFeedLag: number | null = null;
  let resyncs = 0;
  const degraded: DegradedOutput[] = [];
  const shards = new Map<string, GroupShard>();
  const statusOf = new Map(
    (shardStatuses ?? []).map((status) => [`${status.feedId}#${status.shardIndex}`, status]),
  );
  for (const output of outputs) {
    if (!members.has(output.id)) continue;
    if (isDelivering(output)) delivering += 1;
    if (output.diagnosis) {
      degraded.push({
        name: output.name,
        layer: output.diagnosis.layer,
        cause: describeCause(output.diagnosis.cause),
        ruledOut: output.diagnosis.ruledOut,
      });
    }
    const egress = running.get(output.id);
    if (!egress) continue;
    const ratio = finite(egress.quality?.deliveryRatio);
    if (ratio !== null) minDeliveryRatio = Math.min(minDeliveryRatio ?? ratio, ratio);
    const lag = finite(egress.feedLagUnits);
    if (lag !== null) worstFeedLag = Math.max(worstFeedLag ?? lag, lag);
    resyncs += finite(egress.resyncCount) ?? 0;
    const shardIndex = finite(egress.shardId);
    if (egress.fabricFeed && shardIndex !== null) {
      const key = `${egress.fabricFeed}#${shardIndex}`;
      if (!shards.has(key)) {
        shards.set(key, {
          feedId: egress.fabricFeed,
          shardIndex,
          status: statusOf.get(key) ?? null,
        });
      }
    }
  }
  degraded.sort((a, b) => a.name.localeCompare(b.name));
  return {
    outputs: group.outputs,
    delivering,
    minDeliveryRatio,
    worstFeedLag,
    resyncs,
    shards: [...shards.values()].sort(
      (a, b) => a.feedId.localeCompare(b.feedId) || a.shardIndex - b.shardIndex,
    ),
    degraded,
  };
}
