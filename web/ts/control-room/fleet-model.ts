// Fleet (mission control) from the revisioned runtime state: useful delivery
// first, then the degraded outputs grouped by shared cause and ranked by how
// many outputs each cause takes down.

import type {
  Cause,
  Layer,
  OutputState,
  PipelineState,
  RuntimeState,
} from "../core/generated/runtime-state.js";
import type { TelemetryRates } from "./rates.js";

export interface FleetPipelineRow {
  readonly id: string;
  readonly name: string;
  /** `null` when no ingest is connected. */
  readonly source: { protocol: string; videoCodec: string | null } | null;
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
    for (const output of outputs) {
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
    if (pipeline.ingest !== null && delivering === wanted) pipelinesHealthy += 1;
    outputsWanted += wanted;
    outputsDelivering += delivering;
    outputsPending += pending;
    if (egressBps !== null) fleetEgressBps = (fleetEgressBps ?? 0) + egressBps;
    rows.push({
      id: pipeline.id,
      name: pipeline.name,
      source: pipeline.ingest && {
        protocol: pipeline.ingest.protocol,
        videoCodec: pipeline.ingest.videoCodec,
      },
      ingestBps: rates?.ingestBps.get(pipeline.id) ?? null,
      egressBps,
      wanted,
      delivering,
      attention,
      pending,
    });
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

/** One operator sentence for a diagnosis cause. */
export function describeCause(cause: Cause): string {
  switch (cause.code) {
    case "noPublisher":
      return "no publisher connected";
    case "stageNotProducing":
      return `stage ${cause.stage} not producing (${cause.phase})`;
    case "shardPanicked":
      return `egress shard ${cause.shard} panicked`;
    case "shardStalled":
      return `egress shard ${cause.shard} stalled`;
    case "shardStopped":
      return `egress shard ${cause.shard} stopped`;
    case "retriesExhausted":
      return "retries exhausted";
    case "retryBudgetNearlySpent":
      return `retrying · attempt ${cause.attempts} / ${cause.max}`;
    case "rejected":
      return cause.phase
        ? `rejected at ${cause.phase}: ${cause.error}`
        : `rejected: ${cause.error}`;
    case "backpressured":
      return cause.stalled ? "backpressured and stalled" : "backpressured";
    case "noProgress":
      return "no progress";
  }
}
