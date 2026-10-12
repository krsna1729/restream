// The Inspect workspace's model: one pipeline's resource map (who owns each
// node, what it holds, how sure the number is), its source ring and the
// egress shards that carry its outputs.

import type {
  GraphData,
  ResourceMapNode,
  ResourceMapSnapshot,
} from "../core/api-types.js";
import type { OutputState } from "../core/generated/runtime-state.js";

export interface ResourceRow {
  readonly node: ResourceMapNode;
  readonly memoryBytes: number | null;
  /** `measured`, `derived`, `estimated` or `unmeasured`. */
  readonly confidence: string | null;
  readonly hotspots: readonly string[];
}

export interface RingView {
  readonly fill: number;
  readonly capacity: number;
  readonly payloadBytes: number | null;
}

export interface ShardView {
  readonly id: string;
  readonly protocol: string;
  readonly index: number;
  readonly state: string;
  readonly progressAgeMs: number | null;
  readonly commandDepth: number | null;
  readonly commandCapacity: number | null;
  readonly mediaTicks: number | null;
}

function finiteOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** Nodes under pressure first (a hotspot), then by memory held, then name. */
export function buildResourceRows(
  snapshot: ResourceMapSnapshot,
): readonly ResourceRow[] {
  return snapshot.nodes
    .map((node) => ({
      node,
      memoryBytes: finiteOrNull(node.memory?.attributedBytes),
      confidence: node.memory?.confidence ?? null,
      hotspots: node.hotspots ?? [],
    }))
    .sort(
      (a, b) =>
        Number(b.hotspots.length > 0) - Number(a.hotspots.length > 0) ||
        (b.memoryBytes ?? -1) - (a.memoryBytes ?? -1) ||
        a.node.label.localeCompare(b.node.label),
    );
}

/** The pipeline's source ring from the graph; `null` when none runs. */
export function ringFromGraph(graph: GraphData): RingView | null {
  const ring = graph.nodes.find((node) => node.type === "ring_buffer");
  const details = ring?.details;
  if (!details) return null;
  const fill = finiteOrNull(details.fill);
  const capacity = finiteOrNull(details.capacity);
  if (fill === null || capacity === null || capacity <= 0) return null;
  const payload = details.payloadStats as Record<string, unknown> | undefined;
  return {
    fill,
    capacity,
    payloadBytes: finiteOrNull(payload?.payloadBytes),
  };
}

/**
 * The egress shards that carry `outputs`, by protocol then index. Shards are
 * a process-wide resource, so they come from the runtime-wide map; an
 * output's state names the shard it runs on.
 */
export function shardsFrom(
  snapshot: ResourceMapSnapshot,
  outputs: readonly OutputState[],
): readonly ShardView[] {
  const placed = new Set(
    outputs.flatMap((o) => (o.shard === null ? [] : [`${o.protocol}:${o.shard}`])),
  );
  return snapshot.nodes
    .filter(
      (node) =>
        node.kind === "egress_shard" &&
        placed.has(`${node.protocol}:${node.shardIndex}`),
    )
    .map((node) => ({
      id: node.id,
      protocol: node.protocol ?? "unknown",
      index: node.shardIndex ?? -1,
      state: node.state ?? "unknown",
      progressAgeMs: finiteOrNull(node.metrics?.progressAgeMs),
      commandDepth: finiteOrNull(node.metrics?.commandDepth),
      commandCapacity: finiteOrNull(node.metrics?.commandCapacity),
      mediaTicks: finiteOrNull(node.metrics?.mediaTicks),
    }))
    .sort((a, b) => a.protocol.localeCompare(b.protocol) || a.index - b.index);
}
