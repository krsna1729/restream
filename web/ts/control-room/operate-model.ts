// The Operate cockpit's model: the pipeline's runtime graph with shared work
// drawn once (each stage with its consumer count) and destinations folded
// into groups, plus the destination matrix.

import type { GraphData } from "../core/api-types.js";
import type {
  EgressProtocol,
  OutputState,
} from "../core/generated/runtime-state.js";
import { isDelivering } from "./fleet-model.js";

export interface GraphStage {
  readonly id: string;
  readonly type: string;
  readonly label: string;
  readonly active: boolean;
  /** Graph nodes reading this stage's output. */
  readonly consumers: number;
  readonly packetsPerSec: number | null;
}

/** The destinations one upstream stage feeds over one protocol. */
export interface EgressGroup {
  readonly key: string;
  /** `null` for an egress node with no output in the runtime state. */
  readonly protocol: EgressProtocol | null;
  readonly outputs: number;
  readonly delivering: number;
  readonly attention: number;
}

export interface GraphColumn {
  readonly stages: readonly GraphStage[];
  readonly groups: readonly EgressGroup[];
}

export interface DestinationRow {
  readonly output: OutputState;
  readonly delivering: boolean;
  /** From the running egress, when one runs. */
  readonly feedLagUnits: number | null;
}

// The server names an output's egress node `<pipeline>_output_<output>`
// (src/api_runtime_views/graph.rs).
function egressNodeId(pipelineId: string, outputId: string): string {
  return `${pipelineId}_output_${outputId}`;
}

function finiteOrNull(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/**
 * Columns of the graph by longest path from its roots: a stage sits right of
 * every stage it reads, egress groups right of the stage that feeds them.
 */
export function buildMediaGraph(
  graph: GraphData,
  outputs: readonly OutputState[],
): readonly GraphColumn[] {
  const outputByNode = new Map<string, OutputState>();
  for (const output of outputs) {
    outputByNode.set(egressNodeId(graph.pipelineId, output.id), output);
  }
  const incoming = new Map<string, string[]>();
  const consumers = new Map<string, number>();
  for (const edge of graph.edges) {
    const list = incoming.get(edge.to);
    if (list) list.push(edge.from);
    else incoming.set(edge.to, [edge.from]);
    consumers.set(edge.from, (consumers.get(edge.from) ?? 0) + 1);
  }

  const depths = new Map<string, number>();
  const visiting = new Set<string>();
  const depthOf = (id: string): number => {
    const known = depths.get(id);
    if (known !== undefined) return known;
    // A cycle (never sent today) must not hang the view.
    if (visiting.has(id)) return 0;
    visiting.add(id);
    let depth = 0;
    for (const from of incoming.get(id) ?? []) {
      depth = Math.max(depth, depthOf(from) + 1);
    }
    visiting.delete(id);
    depths.set(id, depth);
    return depth;
  };

  const stagesByDepth = new Map<number, GraphStage[]>();
  const groupsByKey = new Map<
    string,
    { depth: number; group: { -readonly [K in keyof EgressGroup]: EgressGroup[K] } }
  >();
  for (const node of graph.nodes) {
    const depth = depthOf(node.id);
    if (node.type !== "egress") {
      const stage: GraphStage = {
        id: node.id,
        type: node.type,
        label: node.label,
        active: node.active,
        consumers: consumers.get(node.id) ?? 0,
        packetsPerSec: finiteOrNull(node.metrics?.packetsPerSec),
      };
      const list = stagesByDepth.get(depth);
      if (list) list.push(stage);
      else stagesByDepth.set(depth, [stage]);
      continue;
    }
    const output = outputByNode.get(node.id);
    const protocol = output?.protocol ?? null;
    const upstream = [...(incoming.get(node.id) ?? [])].sort().join(",");
    const key = `${upstream}|${protocol ?? ""}`;
    let entry = groupsByKey.get(key);
    if (!entry) {
      entry = {
        depth,
        group: { key, protocol, outputs: 0, delivering: 0, attention: 0 },
      };
      groupsByKey.set(key, entry);
    }
    entry.group.outputs += 1;
    if (output && isDelivering(output)) entry.group.delivering += 1;
    if (output?.diagnosis) entry.group.attention += 1;
  }

  const depthsInUse = new Set<number>(stagesByDepth.keys());
  for (const { depth } of groupsByKey.values()) depthsInUse.add(depth);
  return [...depthsInUse]
    .sort((a, b) => a - b)
    .map((depth) => ({
      stages: stagesByDepth.get(depth) ?? [],
      groups: [...groupsByKey.values()]
        .filter((entry) => entry.depth === depth)
        .map((entry) => entry.group),
    }));
}

/** The pipeline's outputs, degraded first, then the wanted ones not yet
 * delivering, then by name. */
export function buildDestinationRows(
  pipelineId: string,
  outputs: readonly OutputState[],
  graph: GraphData | null,
): readonly DestinationRow[] {
  const details = new Map<string, Record<string, unknown>>();
  for (const node of graph?.nodes ?? []) {
    if (node.type === "egress" && node.details) details.set(node.id, node.details);
  }
  const rank = (row: DestinationRow): number => {
    if (row.output.diagnosis) return 0;
    if (row.output.desired === "running" && !row.delivering) return 1;
    return 2;
  };
  return outputs
    .filter((output) => output.pipelineId === pipelineId)
    .map((output) => {
      const running = details.get(egressNodeId(pipelineId, output.id));
      return {
        output,
        delivering: isDelivering(output),
        feedLagUnits: finiteOrNull(running?.feedLagUnits),
      };
    })
    .sort(
      (a, b) =>
        rank(a) - rank(b) || a.output.name.localeCompare(b.output.name),
    );
}
