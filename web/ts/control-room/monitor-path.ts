// Monitor's per-pipeline panels below the correlated signals: the selected
// input's transport, the media path its outputs share, and the destination
// that delivers least.

import type { GraphData, TelemetryEgress } from "../core/api-types.js";
import type { OutputState } from "../core/generated/runtime-state.js";
import type { PipelineInput, PublisherQuality } from "../types.js";
import { formatBps, formatPercent } from "./format.js";
import { ringFromGraph } from "./inspect-model.js";

/** Below this delivery ratio an output is not keeping up (the engine's
 * per-feed delivery floor, `feed_delivery` in `api/runtime_views/telemetry.rs`). */
export const DELIVERY_FLOOR = 0.95;

function finite(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/**
 * The selected input's transport, one labeled value per metric its protocol
 * reports; a metric the publisher does not report is left out, not shown as 0.
 */
export function inputTransport(
  inputs: readonly PipelineInput[],
): { label: string; protocol: string; rows: ReadonlyArray<[string, string]> } | null {
  const input = inputs.find((candidate) => candidate.selected && candidate.runtime.connected);
  if (!input) return null;
  const quality: PublisherQuality = input.runtime.quality ?? {};
  const rows: Array<[string, string]> = [];
  const add = (label: string, value: number | null, format: (value: number) => string) => {
    if (value !== null) rows.push([label, format(value)]);
  };
  add("RTT", finite(quality.msRtt) ?? finite(quality.tcpRttMs), (ms) => `${ms.toFixed(0)} ms`);
  add(
    "Receive rate",
    finite(quality.mbpsReceiveRate) ?? finite(quality.tcpReceiveRateMbps),
    (mbps) => formatBps(mbps * 1_000_000),
  );
  const buffered = finite(quality.srtRecvBufPackets);
  const capacity = finite(quality.srtRecvBufCapacityPackets);
  add(
    "Receive buffer",
    buffered !== null && capacity !== null && capacity > 0 ? (100 * buffered) / capacity : null,
    formatPercent,
  );
  add("Loss / s", finite(quality.packetsReceivedLossPerSec), (n) => n.toFixed(1));
  add("Drop / s", finite(quality.packetsReceivedDropPerSec), (n) => n.toFixed(1));
  add("Retrans / s", finite(quality.packetsReceivedRetransPerSec), (n) => n.toFixed(1));
  if (quality.srtBonded) {
    const connected = finite(quality.srtGroupConnectedMembers);
    const members = finite(quality.srtGroupMemberCount);
    if (connected !== null && members !== null) {
      rows.push(["Bonded links", `${connected} / ${members} connected`]);
    }
  }
  return {
    label: input.label,
    protocol: (input.runtime.protocol ?? "unknown").toUpperCase(),
    rows,
  };
}

export interface PathStage {
  readonly stageKey: string;
  readonly label: string;
  readonly active: boolean;
  readonly packetsPerSec: number | null;
  /** Wanted outputs whose running attempt reads this stage last. */
  readonly outputs: number;
}

export interface MediaPath {
  readonly ring: {
    readonly fill: number;
    readonly capacity: number;
    /** Times any reader fell off the ring, counted once: a stage node's
     * `details.overflowCount` repeats its reader's counter. */
    readonly overflows: number;
    readonly worstLagSlots: number | null;
  } | null;
  readonly stages: readonly PathStage[];
}

/** The source ring and the stages the pipeline's outputs share, from its
 * processing graph; `outputs` counts the readers each stage serves. */
export function mediaPath(graph: GraphData, outputs: readonly OutputState[]): MediaPath {
  const ringNode = graph.nodes.find((node) => node.type === "ring_buffer");
  const ring = ringFromGraph(graph);
  const readers = Array.isArray(ringNode?.details?.readers)
    ? (ringNode.details.readers as ReadonlyArray<Record<string, unknown>>)
    : [];
  let worstLagSlots: number | null = null;
  let overflows = 0;
  for (const reader of readers) {
    overflows += finite(reader.overflowCount) ?? 0;
    const lag = finite(reader.lagSlots);
    if (lag !== null) worstLagSlots = Math.max(worstLagSlots ?? lag, lag);
  }
  const readersOf = new Map<string, number>();
  for (const output of outputs) {
    if (output.pipelineId !== graph.pipelineId) continue;
    if (output.desired !== "running" || output.stage === null) continue;
    readersOf.set(output.stage, (readersOf.get(output.stage) ?? 0) + 1);
  }
  const stages = graph.nodes
    .filter((node) => node.stageKey && !node.outputId && node.type !== "ring_buffer")
    .map((node) => ({
      stageKey: node.stageKey ?? "",
      label: node.label,
      active: node.active,
      packetsPerSec: finite(node.metrics?.packetsPerSec),
      outputs: readersOf.get(node.stageKey ?? "") ?? 0,
    }))
    .sort((a, b) => b.outputs - a.outputs || a.label.localeCompare(b.label));
  return {
    ring: ring && { fill: ring.fill, capacity: ring.capacity, overflows, worstLagSlots },
    stages,
  };
}

export interface Outlier {
  readonly outputId: string;
  readonly name: string;
  readonly ratio: number;
  readonly offeredBps: number | null;
  readonly deliveredBps: number | null;
  readonly feedLag: number | null;
  /** The median ratio of the other rated outputs reading the same stage
   * over the same protocol; `null` when it has none. */
  readonly peersRatio: number | null;
}

/**
 * The pipeline's wanted output that delivers least, when it is under the
 * delivery floor; `null` when every rated output keeps up. Peers are the
 * outputs the engine rates together (same stage, same protocol: different
 * wire framing would read as unfairness).
 */
export function destinationOutlier(
  pipelineId: string,
  outputs: readonly OutputState[],
  egresses: readonly TelemetryEgress[],
): Outlier | null {
  const egressOf = new Map(egresses.map((egress) => [egress.outputId, egress]));
  const rated = outputs.flatMap((output) => {
    if (output.pipelineId !== pipelineId || output.desired !== "running") return [];
    const egress = egressOf.get(output.id);
    const ratio = finite(egress?.quality?.deliveryRatio);
    return egress && ratio !== null ? [{ output, egress, ratio }] : [];
  });
  let worst = rated[0];
  for (const candidate of rated) {
    if (worst === undefined || candidate.ratio < worst.ratio) worst = candidate;
  }
  if (worst === undefined || worst.ratio >= DELIVERY_FLOOR) return null;
  const peers = rated
    .filter(
      (peer) =>
        peer !== worst &&
        worst.output.stage !== null &&
        peer.output.stage === worst.output.stage &&
        peer.output.protocol === worst.output.protocol,
    )
    .map((peer) => peer.ratio)
    .sort((a, b) => a - b);
  const middle = Math.floor(peers.length / 2);
  return {
    outputId: worst.output.id,
    name: worst.output.name,
    ratio: worst.ratio,
    offeredBps: finite(worst.egress.quality?.offeredBps),
    deliveredBps: finite(worst.egress.quality?.deliveredBps),
    feedLag: finite(worst.egress.feedLagUnits),
    peersRatio:
      peers.length === 0
        ? null
        : peers.length % 2 === 1
          ? (peers[middle] ?? null)
          : ((peers[middle - 1] ?? 0) + (peers[middle] ?? 0)) / 2,
  };
}
