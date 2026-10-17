// One pipeline's inputs and processing graph for the views, read from the
// live store: the structure and state from the runtime state stream, the
// counters (rates, fill, lag, transport quality) from the store's telemetry
// sample. Nothing here polls.

import type { EngineTelemetrySnapshot, GraphData, GraphNode } from "../core/api-types.js";
import type { RuntimeState } from "../core/state-stream.js";
import type { PipelineInput } from "../types.js";

/**
 * The pipeline's inputs in list order (the primary first, then backups by
 * age), each with its session's counters from `telemetry`; `null` until the
 * state arrives.
 */
export function pipelineInputs(
  state: RuntimeState | null,
  telemetry: EngineTelemetrySnapshot | null,
  pipelineId: string,
): PipelineInput[] | null {
  if (state === null) return null;
  const counters = new Map((telemetry?.inputs ?? []).map((row) => [row.inputId, row]));
  return state.inputs
    .filter((input) => input.pipelineId === pipelineId)
    .sort((a, b) => a.position - b.position)
    .map(({ position: _position, runtime, ...input }) => {
      // A sample from before a disconnect describes a session that is gone.
      const sampled = runtime.connected ? counters.get(input.id) : undefined;
      return {
        ...input,
        runtime: {
          ...runtime,
          uptimeSeconds: sampled?.uptimeSecs ?? null,
          bytesReceived: sampled?.bytesReceived ?? 0,
          quality: sampled?.quality ?? null,
        },
      };
    });
}

/**
 * The pipeline's processing graph with each node's counters attached: ingest
 * and demux from the ingest row, a stage from its stage row, the source ring
 * from its ring row, an egress from its output's row. A node the sample does
 * not cover has none. `null` until the state holds the pipeline.
 */
export function pipelineGraph(
  state: RuntimeState | null,
  telemetry: EngineTelemetrySnapshot | null,
  pipelineId: string,
): GraphData | null {
  const graph = state?.graphs.find((candidate) => candidate.pipelineId === pipelineId);
  if (!graph) return null;
  const stages = new Map(
    (telemetry?.stages ?? []).flatMap((row) => (row.stageKey ? [[row.stageKey, row]] : [])),
  );
  const egresses = new Map((telemetry?.egresses ?? []).map((row) => [row.outputId, row]));
  const ingest = telemetry?.ingests.find((row) => row.pipelineId === pipelineId);
  const ring = telemetry?.memoryAccounting?.sourceRings?.find(
    (row) => row.pipelineId === pipelineId,
  );
  return {
    pipelineId,
    edges: graph.edges,
    nodes: graph.nodes.map((node): GraphNode => {
      const view: GraphNode = {
        id: node.id,
        type: node.type,
        label: node.label,
        active: node.active,
        ...(node.stageKey !== null && { stageKey: node.stageKey }),
        ...(node.outputId !== null && { outputId: node.outputId }),
      };
      if (node.type === "ingest" || node.type === "demux") {
        return { ...view, metrics: ingest?.metrics ?? null };
      }
      if (node.type === "ring_buffer") {
        return { ...view, details: ring ? { ...ring } : null };
      }
      if (node.outputId !== null) {
        const egress = egresses.get(node.outputId);
        return { ...view, details: egress ? { ...egress } : null, metrics: egress?.metrics ?? null };
      }
      const row = node.metricsStage === null ? undefined : stages.get(node.metricsStage);
      return { ...view, metrics: row?.metrics ?? null };
    }),
  };
}
