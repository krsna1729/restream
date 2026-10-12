// The Incidents workspace's model. An incident is an open alert: a period
// (first seen .. last seen) with an affected entity, a diagnosis and an
// operator action. The engine keeps no incident history, so alerts that
// close while the page is open are kept here as resolved.

import type {
  AlertScope,
  LifecycleEvent,
  OperatorAlert,
} from "../core/api-types.js";
import type { Layer } from "../core/generated/runtime-state.js";
import { describeCause } from "./fleet-model.js";

export interface ResolvedIncident {
  readonly alert: OperatorAlert;
  /** Wall-clock ms of the first poll that no longer listed it. */
  readonly resolvedAtMs: number;
}

export interface EvidencePacket {
  /** The degraded layer and why; `null` when the alert has no diagnosis. */
  readonly failed: string | null;
  /** Layers checked and healthy upstream of the failure. */
  readonly notFailed: readonly string[];
  readonly evidence: readonly string[];
  readonly action: string;
}

const LAYER_LABELS: Record<Layer, string> = {
  ingest: "ingest (the publisher and its input)",
  feed: "feed (the shared stage that prepares the media)",
  shard: "egress shard (the thread that sends it)",
  destination: "destination (the remote endpoint and the path to it)",
};

// A wider scope affects more: the engine before a pipeline, a pipeline
// before one of its stages or outputs.
const SCOPE_ORDER: Record<AlertScope, number> = {
  engine: 0,
  pipeline: 1,
  stage: 2,
  output: 3,
};

function seenMs(at: string | undefined): number {
  const ms = at ? Date.parse(at) : Number.NaN;
  return Number.isFinite(ms) ? ms : Number.POSITIVE_INFINITY;
}

/** Critical first, then the widest scope, then the oldest. */
export function rankIncidents(
  alerts: readonly OperatorAlert[],
): readonly OperatorAlert[] {
  return [...alerts].sort(
    (a, b) =>
      Number(b.severity === "critical") - Number(a.severity === "critical") ||
      SCOPE_ORDER[a.scope] - SCOPE_ORDER[b.scope] ||
      seenMs(a.firstSeen) - seenMs(b.firstSeen) ||
      a.id.localeCompare(b.id),
  );
}

/**
 * `resolved` with the alerts of `previous` that `current` no longer lists,
 * newest first, keeping `cap`. An alert that reopens leaves the list.
 */
export function updateResolved(
  resolved: readonly ResolvedIncident[],
  previous: readonly OperatorAlert[],
  current: readonly OperatorAlert[],
  atMs: number,
  cap: number,
): readonly ResolvedIncident[] {
  const open = new Set(current.map((alert) => alert.id));
  const closed = previous
    .filter((alert) => !open.has(alert.id))
    .map((alert) => ({ alert, resolvedAtMs: atMs }));
  return [...closed, ...resolved.filter((entry) => !open.has(entry.alert.id))].slice(
    0,
    cap,
  );
}

export function evidencePacket(alert: OperatorAlert): EvidencePacket {
  const diagnosis = alert.diagnosis;
  return {
    failed: diagnosis
      ? `${LAYER_LABELS[diagnosis.layer]}: ${describeCause(diagnosis.cause)}`
      : null,
    notFailed: diagnosis?.ruledOut.map((layer) => LAYER_LABELS[layer]) ?? [],
    evidence: alert.evidence,
    action: alert.recommendedAction,
  };
}

/** How far before an incident opened its timeline starts. */
export const TIMELINE_LEAD_MS = 60_000;

/**
 * The lifecycle events of the alert's pipeline (and, for an output alert,
 * that output or the pipeline as a whole) from a minute before it opened,
 * oldest first. Without an open time, every event may matter.
 */
export function incidentTimeline(
  alert: OperatorAlert,
  events: readonly LifecycleEvent[],
): readonly LifecycleEvent[] {
  const opened = seenMs(alert.firstSeen);
  const fromMs = Number.isFinite(opened)
    ? opened - TIMELINE_LEAD_MS
    : Number.NEGATIVE_INFINITY;
  return events
    .filter(
      (event) =>
        (alert.pipelineId === undefined || event.pipelineId === alert.pipelineId) &&
        (alert.outputId === undefined ||
          event.outputId === undefined ||
          event.outputId === alert.outputId) &&
        Date.parse(event.timestamp) >= fromMs,
    )
    .sort((a, b) => a.seq - b.seq);
}
