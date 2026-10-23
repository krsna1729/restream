// Fleet (mission control) from the revisioned runtime state: useful delivery
// first, then the degraded outputs grouped by shared cause and ranked by how
// many outputs each cause takes down.

import type {
  Cause,
  EgressProtocol,
  Layer,
  OutputState,
  PipelineState,
  RuntimeState,
} from "../core/generated/runtime-state.js";
import type { TelemetryRates } from "./rates.js";
import { explainCause } from "./trouble-model.js";

export interface FleetPipelineRow {
  readonly id: string;
  readonly name: string;
  /** `null` when no ingest is connected; `bonded` for an SRT group. */
  readonly source: {
    protocol: string;
    videoCodec: string | null;
    bonded: boolean;
  } | null;
  /** Connected inputs held in warm standby. */
  readonly standbyInputs: number;
  /** A recording file is being written. */
  readonly recording: boolean;
  readonly ingestBps: number | null;
  readonly egressBps: number | null;
  /** Outputs the operator wants running. */
  readonly wanted: number;
  /** Wanted outputs that converged and carry media. */
  readonly delivering: number;
  /** Wanted outputs with a diagnosed fault. */
  readonly attention: number;
  /** Wanted outputs neither delivering nor diagnosed: starting, or not
   * flowing yet. */
  readonly pending: number;
  /** Every output, wanted or not, by protocol. */
  readonly protocols: ReadonlyMap<EgressProtocol, number>;
}

export interface AttentionItem {
  /** Stable across samples: pipeline, layer and the whole cause (two blocked
   * stages are two causes); per output for destination faults. */
  readonly key: string;
  readonly pipelineId: string;
  readonly pipelineName: string;
  readonly layer: Layer;
  readonly cause: Cause;
  readonly ruledOut: readonly Layer[];
  readonly outputs: readonly string[];
}

export interface FleetModel {
  readonly pipelines: readonly FleetPipelineRow[];
  readonly attention: readonly AttentionItem[];
  readonly pipelinesTotal: number;
  readonly pipelinesHealthy: number;
  readonly pipelinesReceiving: number;
  readonly outputsWanted: number;
  readonly outputsDelivering: number;
  readonly outputsPending: number;
  readonly egressBps: number | null;
}

export function isDelivering(output: OutputState): boolean {
  return (
    output.desired === "running" &&
    output.convergence === "converged" &&
    output.flow === "flowing"
  );
}

// Upstream first: one ingest fault outranks a destination fault of the same
// blast radius, as it explains more of the plant.
const LAYER_ORDER: Record<Layer, number> = {
  ingest: 0,
  feed: 1,
  shard: 2,
  destination: 3,
};

export function buildFleetModel(
  state: RuntimeState,
  rates: TelemetryRates | null,
): FleetModel {
  const outputsByPipeline = new Map<string, OutputState[]>();
  for (const output of state.outputs) {
    const list = outputsByPipeline.get(output.pipelineId);
    if (list) list.push(output);
    else outputsByPipeline.set(output.pipelineId, [output]);
  }

  const attentionByKey = new Map<string, AttentionItem & { outputs: string[] }>();
  const rows: FleetPipelineRow[] = [];
  let pipelinesHealthy = 0;
  let pipelinesReceiving = 0;
  let outputsWanted = 0;
  let outputsDelivering = 0;
  let outputsPending = 0;
  let fleetEgressBps: number | null = null;

  for (const pipeline of state.pipelines) {
    const outputs = outputsByPipeline.get(pipeline.id) ?? [];
    let wanted = 0;
    let delivering = 0;
    let attention = 0;
    let pending = 0;
    let egressBps: number | null = null;
    const protocols = new Map<EgressProtocol, number>();
    for (const output of outputs) {
      protocols.set(output.protocol, (protocols.get(output.protocol) ?? 0) + 1);
      const rate = rates?.egressBps.get(output.id);
      if (rate !== undefined) egressBps = (egressBps ?? 0) + rate;
      if (output.desired !== "running") continue;
      wanted += 1;
      const flowing = isDelivering(output);
      if (flowing) delivering += 1;
      if (output.diagnosis !== null) {
        attention += 1;
        addAttention(attentionByKey, pipeline, output);
      } else if (!flowing) {
        pending += 1;
      }
    }
    if (pipeline.ingest !== null) pipelinesReceiving += 1;
    outputsWanted += wanted;
    outputsDelivering += delivering;
    outputsPending += pending;
    if (egressBps !== null) fleetEgressBps = (fleetEgressBps ?? 0) + egressBps;
    const row: FleetPipelineRow = {
      id: pipeline.id,
      name: pipeline.name,
      source: pipeline.ingest && {
        protocol: pipeline.ingest.protocol,
        videoCodec: pipeline.ingest.videoCodec,
        bonded: pipeline.ingest.bonded,
      },
      standbyInputs: pipeline.standbyInputs,
      recording: pipeline.recording,
      ingestBps: rates?.ingestBps.get(pipeline.id) ?? null,
      egressBps,
      wanted,
      delivering,
      attention,
      pending,
      protocols,
    };
    // The stat and the row badge share one rule.
    if (pipelineHealth(row).tone === "ok") pipelinesHealthy += 1;
    rows.push(row);
  }

  // Attention first, then the pipelines that carry the most outputs.
  rows.sort(
    (a, b) =>
      b.attention - a.attention ||
      b.wanted - a.wanted ||
      a.name.localeCompare(b.name),
  );
  const attention = [...attentionByKey.values()].sort(
    (a, b) =>
      b.outputs.length - a.outputs.length ||
      LAYER_ORDER[a.layer] - LAYER_ORDER[b.layer] ||
      a.key.localeCompare(b.key),
  );

  return {
    pipelines: rows,
    attention,
    pipelinesTotal: state.pipelines.length,
    pipelinesHealthy,
    pipelinesReceiving,
    outputsWanted,
    outputsDelivering,
    outputsPending,
    egressBps: fleetEgressBps,
  };
}

function addAttention(
  byKey: Map<string, AttentionItem & { outputs: string[] }>,
  pipeline: PipelineState,
  output: OutputState,
): void {
  const diagnosis = output.diagnosis;
  if (diagnosis === null) return;
  // Destination faults are per output; upstream faults are shared by every
  // output of the pipeline that reports them.
  const scope = diagnosis.layer === "destination" ? `:${output.id}` : "";
  const key = `${pipeline.id}:${diagnosis.layer}:${JSON.stringify(diagnosis.cause)}${scope}`;
  const existing = byKey.get(key);
  if (existing) {
    existing.outputs.push(output.name);
    return;
  }
  byKey.set(key, {
    key,
    pipelineId: pipeline.id,
    pipelineName: pipeline.name,
    layer: diagnosis.layer,
    cause: diagnosis.cause,
    ruledOut: diagnosis.ruledOut,
    outputs: [output.name],
  });
}

export interface PipelineHealth {
  readonly tone: "warning" | "info" | "idle" | "ok";
  readonly label: string;
}

/** A row is healthy only when every wanted output delivers. */
export function pipelineHealth(row: FleetPipelineRow): PipelineHealth {
  if (row.attention > 0) {
    return { tone: "warning", label: `${row.attention} degraded` };
  }
  if (row.pending > 0) {
    return { tone: "info", label: `${row.pending} starting` };
  }
  if (row.source === null) return { tone: "idle", label: "idle" };
  return { tone: "ok", label: "healthy" };
}

const HEALTH_RANK: Record<PipelineHealth["tone"], number> = {
  warning: 0,
  info: 1,
  ok: 2,
  idle: 3,
};

/** Degraded first, then starting, healthy, idle; within one, the pipeline
 * with the most wanted outputs first, then by name. */
export function worstFirst(rows: readonly FleetPipelineRow[]): readonly FleetPipelineRow[] {
  return [...rows].sort(
    (a, b) =>
      HEALTH_RANK[pipelineHealth(a).tone] - HEALTH_RANK[pipelineHealth(b).tone] ||
      b.wanted - a.wanted ||
      a.name.localeCompare(b.name),
  );
}

/** A diagnosed cause in a few plain words (`trouble-model.ts` has the fix). */
export function describeCause(cause: Cause): string {
  return explainCause(cause).summary;
}

export type HealthFilter = PipelineHealth["tone"] | "all";

/** The operational cards: pipelines that need attention, have a warm
 * standby, ingest over bonded SRT, or record. */
export type OperationalFilter = "attention" | "standby" | "bonded" | "recording";

export const OPERATIONAL: Record<
  OperationalFilter,
  (row: FleetPipelineRow) => boolean
> = {
  attention: (row) => row.attention > 0,
  standby: (row) => row.standbyInputs > 0,
  bonded: (row) => row.source?.bonded === true,
  recording: (row) => row.recording,
};

/** Rows whose name or id holds `query` (any case), whose health is
 * `health`, and that match the operational card, when one is chosen. */
export function filterPipelineRows(
  rows: readonly FleetPipelineRow[],
  query: string,
  health: HealthFilter,
  operational: OperationalFilter | null = null,
): readonly FleetPipelineRow[] {
  const needle = query.trim().toLowerCase();
  return rows.filter(
    (row) =>
      (needle === "" ||
        row.name.toLowerCase().includes(needle) ||
        row.id.toLowerCase().includes(needle)) &&
      (health === "all" || pipelineHealth(row).tone === health) &&
      (operational === null || OPERATIONAL[operational](row)),
  );
}

/** Attention items that share a layer and a kind of cause, across
 * pipelines: "12 pipelines have no publisher", "46 destinations rejected the
 * stream". At thousands of outputs the per-item list is too long to read;
 * the groups say what kind of trouble the plant is in. */
export interface AttentionGroup {
  readonly key: string;
  readonly layer: Layer;
  readonly code: Cause["code"];
  readonly title: string;
  readonly outputs: number;
  readonly pipelines: number;
  /** Worst first: the items with the most outputs. */
  readonly items: readonly AttentionItem[];
}

const GROUP_TITLE: Record<Cause["code"], string> = {
  noPublisher: "No publisher connected",
  stageNotProducing: "A shared stage is not producing",
  shardPanicked: "An egress shard panicked",
  shardStalled: "An egress shard stalled",
  shardStopped: "An egress shard stopped",
  capacityRefused: "Egress capacity is full",
  retriesExhausted: "Retries exhausted",
  retryBudgetNearlySpent: "Retry budget nearly spent",
  rejected: "Destination rejected the stream",
  backpressured: "Destination is not keeping up",
  noProgress: "No progress",
};

export function attentionGroups(items: readonly AttentionItem[]): readonly AttentionGroup[] {
  const groups = new Map<string, { layer: Layer; code: Cause["code"]; items: AttentionItem[] }>();
  for (const item of items) {
    const key = `${item.layer}:${item.cause.code}`;
    const group = groups.get(key);
    if (group) group.items.push(item);
    else groups.set(key, { layer: item.layer, code: item.cause.code, items: [item] });
  }
  return [...groups.entries()]
    .map(([key, group]) => ({
      key,
      layer: group.layer,
      code: group.code,
      title: GROUP_TITLE[group.code],
      outputs: group.items.reduce((sum, item) => sum + item.outputs.length, 0),
      pipelines: new Set(group.items.map((item) => item.pipelineId)).size,
      items: group.items,
    }))
    .sort(
      (a, b) =>
        b.outputs - a.outputs ||
        LAYER_ORDER[a.layer] - LAYER_ORDER[b.layer] ||
        a.key.localeCompare(b.key),
    );
}
