// The Inspect workspace's model: one pipeline's resource map (who owns each
// node, what it holds, how sure the number is), its source ring and the
// egress shards that carry its outputs.

import type {
  GraphData,
  ResourceMapNode,
  ResourceMapSnapshot,
} from "../core/api-types.js";

/** The most nodes a map request asks for (the server's maximum); the rest
 * are counted in `limits.truncatedNodeCount`, not listed. */
export const MAP_TOP_N = 200;

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
  /** The prepared feed the shard serves: one per pipeline rendition. */
  readonly feed: string;
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
 * The egress shards in a pipeline's map: the server lists the ones its
 * running outputs are placed on. By protocol, feed, then index (every feed
 * numbers its shards from 0).
 */
export function shardsFrom(snapshot: ResourceMapSnapshot): readonly ShardView[] {
  return snapshot.nodes
    .filter((node) => node.kind === "egress_shard")
    .map((node) => ({
      id: node.id,
      protocol: node.protocol ?? "unknown",
      feed: node.feedId ?? "unknown",
      index: node.shardIndex ?? -1,
      state: node.state ?? "unknown",
      progressAgeMs: finiteOrNull(node.metrics?.progressAgeMs),
      commandDepth: finiteOrNull(node.metrics?.commandDepth),
      commandCapacity: finiteOrNull(node.metrics?.commandCapacity),
      mediaTicks: finiteOrNull(node.metrics?.mediaTicks),
    }))
    .sort(
      (a, b) =>
        a.protocol.localeCompare(b.protocol) ||
        a.feed.localeCompare(b.feed) ||
        a.index - b.index,
    );
}

export interface NodeCard {
  readonly label: string;
  readonly value: string;
  readonly foot: string;
}

/**
 * The selected node's headline numbers. Consumers come from the graph (the
 * graph node of the same stage, or the source ring); CPU only where the
 * engine measures it; packet rate and work per packet from the node's
 * counters, averaged since it started. A number the node does not have is
 * left out rather than shown as zero.
 */
export function nodeCards(
  node: ResourceMapNode,
  graph: GraphData | null,
): readonly NodeCard[] {
  const cards: NodeCard[] = [];
  const graphNode = graph?.nodes.find((candidate) =>
    node.kind === "source_ring"
      ? candidate.type === "ring_buffer"
      : candidate.stageKey === node.id,
  );
  if (graph && graphNode) {
    const consumers = graph.edges.filter((edge) => edge.from === graphNode.id).length;
    cards.push({ label: "Consumers", value: String(consumers), foot: "graph nodes reading it" });
  }
  const cpu = finiteOrNull(node.cpuPercent);
  if (cpu !== null) {
    cards.push({
      label: "CPU",
      value: `${cpu.toFixed(1)}%`,
      foot:
        node.backendPid !== undefined
          ? `measured child PID ${node.backendPid}`
          : "measured",
    });
  }
  const rate = finiteOrNull(node.metrics?.packetsPerSec);
  if (rate !== null) {
    cards.push({
      label: "Packet rate",
      value: `${Math.round(rate).toLocaleString("en-US")}/s`,
      foot: "input packets, since it started",
    });
  }
  const work = finiteOrNull(node.metrics?.avgUsPerPacket);
  if (work !== null && work > 0) {
    cards.push({
      label: "Average work",
      value: `${work.toFixed(work < 10 ? 1 : 0)} µs`,
      foot: "per input packet",
    });
  }
  return cards;
}
