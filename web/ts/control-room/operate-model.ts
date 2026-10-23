// The Operate cockpit's model: the pipeline's runtime graph with shared work
// drawn once (each stage with its consumer count) and destinations folded
// into groups, plus the destination matrix.

import type { GraphData, YoutubeMonitoringStatus } from "../core/api-types.js";
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
  /** The labels of the stages that feed the group. */
  readonly upstream: string;
  /** The group's outputs in the runtime state. */
  readonly outputIds: readonly string[];
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
  const outputById = new Map(outputs.map((output) => [output.id, output]));
  const labels = new Map(graph.nodes.map((node) => [node.id, node.label]));
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
    {
      depth: number;
      ids: string[];
      group: { -readonly [K in keyof EgressGroup]: EgressGroup[K] };
    }
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
    const output = node.outputId ? outputById.get(node.outputId) : undefined;
    const protocol = output?.protocol ?? null;
    const upstreamIds = [...(incoming.get(node.id) ?? [])].sort();
    const key = `${upstreamIds.join(",")}|${protocol ?? ""}`;
    let entry = groupsByKey.get(key);
    if (!entry) {
      const ids: string[] = [];
      entry = {
        depth,
        ids,
        group: {
          key,
          protocol,
          upstream: upstreamIds.map((id) => labels.get(id) ?? id).join(" + "),
          outputIds: ids,
          outputs: 0,
          delivering: 0,
          attention: 0,
        },
      };
      groupsByKey.set(key, entry);
    }
    entry.group.outputs += 1;
    if (output) entry.ids.push(output.id);
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

/** Why a row sorts where it does: a diagnosed destination first, then one
 * that is wanted but not delivering, then everything else. */
export function destinationRank(row: DestinationRow): number {
  if (row.output.diagnosis) return 0;
  if (row.output.desired === "running" && !row.delivering) return 1;
  return 2;
}

/** The pipeline's outputs, degraded first, then the wanted ones not yet
 * delivering, then by name. */
export function buildDestinationRows(
  pipelineId: string,
  outputs: readonly OutputState[],
  graph: GraphData | null,
): readonly DestinationRow[] {
  // Egress nodes name their output; `details` exist while an egress runs.
  const details = new Map<string, Record<string, unknown>>();
  for (const node of graph?.nodes ?? []) {
    if (node.outputId && node.details) details.set(node.outputId, node.details);
  }
  return outputs
    .filter((output) => output.pipelineId === pipelineId)
    .map((output) => {
      const running = details.get(output.id);
      return {
        output,
        delivering: isDelivering(output),
        feedLagUnits: finiteOrNull(running?.feedLagUnits),
      };
    })
    .sort(
      (a, b) =>
        destinationRank(a) - destinationRank(b) ||
        a.output.name.localeCompare(b.output.name),
    );
}

/** A monitoring URL the engine can check on YouTube (watch, share or live
 * page). */
export function isYoutubeUrl(url: string): boolean {
  try {
    const host = new URL(url).hostname.toLowerCase();
    return /(^|\.)youtube\.com$/.test(host) || host === "youtu.be";
  } catch {
    return false;
  }
}

/** The verdict shown for a YouTube check; null when the engine could not
 * read the page. */
export function youtubeVerdict(status: YoutubeMonitoringStatus | null): string {
  if (!status) return "YouTube: could not read the page";
  const state = status.live_now
    ? "live now"
    : status.upcoming
      ? "scheduled, not live yet"
      : "not live";
  return `YouTube: ${state}`;
}

/** The destination matrix's state filter. */
export type DestinationFilter = "all" | "attention" | "retrying" | "starting" | "delivering" | "stopped";

/** Whether `row` is in `filter`'s set. Retrying is a subset of attention
 * when the engine diagnosed it; it is offered on its own because "what is
 * reconnecting right now" is a question operators ask. */
export function inDestinationFilter(row: DestinationRow, filter: DestinationFilter): boolean {
  const output = row.output;
  switch (filter) {
    case "all":
      return true;
    case "attention":
      return output.diagnosis !== null;
    case "retrying":
      return output.convergence === "retrying" || output.convergence === "retriesExhausted";
    case "starting":
      return output.desired === "running" && !row.delivering && output.diagnosis === null;
    case "delivering":
      return row.delivering;
    case "stopped":
      return output.desired === "stopped";
  }
}

export const DESTINATION_FILTERS: readonly DestinationFilter[] = ["all", "attention", "retrying", "starting", "delivering", "stopped"];

/** How many rows each filter holds. */
export function destinationFilterCounts(rows: readonly DestinationRow[]): Record<DestinationFilter, number> {
  const counts = { all: 0, attention: 0, retrying: 0, starting: 0, delivering: 0, stopped: 0 };
  for (const row of rows) {
    for (const filter of DESTINATION_FILTERS) if (inDestinationFilter(row, filter)) counts[filter] += 1;
  }
  return counts;
}

/** Rows in `filter`, on `protocol` (`null`: any), whose name or id holds
 * `query` (any case). The order is kept: degraded first. */
export function filterDestinationRows(
  rows: readonly DestinationRow[],
  filter: DestinationFilter,
  query: string,
  protocol: EgressProtocol | null,
): readonly DestinationRow[] {
  const needle = query.trim().toLowerCase();
  return rows.filter(
    (row) =>
      inDestinationFilter(row, filter) &&
      (protocol === null || row.output.protocol === protocol) &&
      (needle === "" || row.output.name.toLowerCase().includes(needle) || row.output.id.toLowerCase().includes(needle)),
  );
}
