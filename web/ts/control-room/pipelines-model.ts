// Fleet-wide summaries for the Pipelines workspace, derived from the
// runtime state: the protocol mix and the stages outputs share.

import type {
  EgressProtocol,
  OutputState,
  PipelineState,
} from "../core/generated/runtime-state.js";
import type { FleetPipelineRow } from "./fleet-model.js";

export interface ProtocolShare {
  readonly protocol: EgressProtocol;
  readonly outputs: number;
}

/** Every output, wanted or not, by protocol; the largest first. */
export function protocolMix(rows: readonly FleetPipelineRow[]): ProtocolShare[] {
  const totals = new Map<EgressProtocol, number>();
  for (const row of rows) {
    for (const [protocol, count] of row.protocols) {
      totals.set(protocol, (totals.get(protocol) ?? 0) + count);
    }
  }
  return [...totals]
    .map(([protocol, outputs]) => ({ protocol, outputs }))
    .sort((a, b) => b.outputs - a.outputs || a.protocol.localeCompare(b.protocol));
}

export interface StageGroup {
  readonly pipelineId: string;
  readonly pipelineName: string;
  /** The stage kind (`video:720p`, `source`, ...). */
  readonly kind: string;
  readonly outputs: number;
}

export interface SharedStages {
  /** Stages read by two or more running outputs; the largest first. */
  readonly groups: readonly StageGroup[];
  /** Running outputs that read a stage another output also reads. */
  readonly sharedOutputs: number;
  /** Running outputs that read any stage. */
  readonly stagedOutputs: number;
}

/** Running outputs grouped by the stage they read: one stage's work serves
 * every output in its group. */
export function sharedStages(
  pipelines: readonly PipelineState[],
  outputs: readonly OutputState[],
): SharedStages {
  const names = new Map(pipelines.map((pipeline) => [pipeline.id, pipeline.name]));
  const byStage = new Map<string, { pipelineId: string; outputs: number }>();
  let stagedOutputs = 0;
  for (const output of outputs) {
    if (output.stage === null) continue;
    stagedOutputs += 1;
    const group = byStage.get(output.stage);
    if (group) group.outputs += 1;
    else byStage.set(output.stage, { pipelineId: output.pipelineId, outputs: 1 });
  }
  const groups: StageGroup[] = [];
  let sharedOutputs = 0;
  for (const [stage, group] of byStage) {
    if (group.outputs < 2) continue;
    sharedOutputs += group.outputs;
    // A stage key is `<pipeline>:<kind>`.
    const prefix = `${group.pipelineId}:`;
    groups.push({
      pipelineId: group.pipelineId,
      pipelineName: names.get(group.pipelineId) ?? group.pipelineId,
      kind: stage.startsWith(prefix) ? stage.slice(prefix.length) : stage,
      outputs: group.outputs,
    });
  }
  groups.sort(
    (a, b) =>
      b.outputs - a.outputs ||
      a.pipelineName.localeCompare(b.pipelineName) ||
      a.kind.localeCompare(b.kind),
  );
  return { groups, sharedOutputs, stagedOutputs };
}
