// The Incidents workspace's model. An incident is an open alert: a period
// (first seen .. last seen) with an affected entity, a diagnosis and an
// operator action. The engine keeps a bounded history of the ones that
// resolved; the alerts read returns those closed since a given time.

import type {
  AlertEvidence,
  AlertScope,
  LifecycleEvent,
  OperatorAlert,
  ResolvedAlert,
} from "../core/api-types.js";
import type { FleetHistoryWindow, Layer } from "../core/generated/runtime-state.js";
import type { FleetSample } from "./fleet-history.js";
import { describeCause } from "./fleet-model.js";

/** How far back the resolved list reaches. */
export const RESOLVED_WINDOWS = [
  { label: "15m", ms: 15 * 60_000 },
  { label: "1h", ms: 60 * 60_000 },
  { label: "6h", ms: 6 * 60 * 60_000 },
  { label: "24h", ms: 24 * 60 * 60_000 },
] as const;

export interface EvidencePacket {
  /** The degraded layer and why; `null` when the alert has no diagnosis. */
  readonly failed: string | null;
  /** Layers checked and healthy upstream of the failure. */
  readonly notFailed: readonly string[];
  readonly evidence: readonly AlertEvidence[];
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
 * Whether a resolved window may be missing some: it reaches back to or past
 * the time after which the engine's kept history is complete (its start, or
 * the close of the latest alert it dropped; an alert closed at that instant
 * may be gone).
 */
export function resolvedMayBeIncomplete(
  windowStartMs: number,
  completeFrom: string | undefined,
): boolean {
  const from = completeFrom === undefined ? Number.NaN : Date.parse(completeFrom);
  return Number.isFinite(from) && windowStartMs <= from;
}

/**
 * The engine's resolved alerts (newest first) less the ones open again,
 * once per id: the latest closing, so a selection by id is unambiguous.
 */
export function visibleResolved(
  resolved: readonly ResolvedAlert[],
  open: readonly OperatorAlert[],
): readonly ResolvedAlert[] {
  const seen = new Set(open.map((alert) => alert.id));
  return resolved.filter((entry) => {
    if (seen.has(entry.alert.id)) return false;
    seen.add(entry.alert.id);
    return true;
  });
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

/** Where an incident's timeline starts: a minute before it opened, or the
 * beginning when it has no open time. */
export function timelineStartMs(alert: OperatorAlert): number {
  const opened = seenMs(alert.firstSeen);
  return Number.isFinite(opened) ? opened - TIMELINE_LEAD_MS : Number.NEGATIVE_INFINITY;
}

/**
 * The lifecycle events of the alert's pipeline (and, for an output alert,
 * that output or the pipeline as a whole) from a minute before it opened,
 * oldest first. Without an open time, every event may matter.
 */
export function incidentTimeline(
  alert: OperatorAlert,
  events: readonly LifecycleEvent[],
): readonly LifecycleEvent[] {
  const fromMs = timelineStartMs(alert);
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

/**
 * Whether the timeline may be missing the incident's lead-up. The engine
 * evicts its oldest events first across all pipelines, and `fetched` is the
 * newest of this pipeline, so the window is complete exactly when some
 * fetched event is from its start or earlier.
 */
export function timelineMayBeIncomplete(
  alert: OperatorAlert,
  fetched: readonly LifecycleEvent[],
): boolean {
  const oldest = Math.min(...fetched.map((event) => Date.parse(event.timestamp)));
  return !(oldest <= timelineStartMs(alert));
}

/** The chart's cursors: when the incident opened and, once it has, closed. */
export function incidentMarks(alert: OperatorAlert, resolvedAt: string | null): number[] {
  return [alert.firstSeen, resolvedAt ?? undefined]
    .map((at) => seenMs(at))
    .filter(Number.isFinite);
}

/**
 * The span the correlation chart reads from the engine's fleet history: from
 * the timeline's start to a minute after the close, so the close cursor has
 * samples on both sides (`toMs` `null`: open, up to now).
 */
export function correlationWindow(
  alert: OperatorAlert,
  resolvedAt: string | null,
): { fromMs: number; toMs: number | null } {
  return {
    fromMs: timelineStartMs(alert),
    toMs: resolvedAt === null ? null : seenMs(resolvedAt) + TIMELINE_LEAD_MS,
  };
}

/**
 * One incident as a self-contained record an operator can attach to a
 * ticket: the alert as last judged, its evidence packet, the timeline the
 * page shows, and the engine's fleet samples from the timeline's start to
 * the close (or now). `events` is `null` when the timeline was not loaded.
 */
export function evidenceExport(input: {
  alert: OperatorAlert;
  resolvedAt: string | null;
  events: readonly LifecycleEvent[] | null;
  history: readonly FleetSample[];
  nowMs: number;
}) {
  const { alert, resolvedAt, events } = input;
  const fromMs = timelineStartMs(alert);
  const toMs = resolvedAt === null ? input.nowMs : seenMs(resolvedAt);
  return {
    exportedAt: new Date(input.nowMs).toISOString(),
    status: resolvedAt === null ? "open" : "resolved",
    resolvedAt,
    alert,
    packet: evidencePacket(alert),
    timeline:
      events === null
        ? null
        : {
            events: incidentTimeline(alert, events),
            mayBeIncomplete: timelineMayBeIncomplete(alert, events),
          },
    signals: input.history.filter((sample) => sample.atMs >= fromMs && sample.atMs <= toMs),
  };
}

/**
 * A closed span (`toMs`) is final once the server's newest sample, or the
 * start of what it keeps, is at or past its end: every sample in it has been
 * taken. Both are server times, so this browser's clock does not matter.
 */
export function fleetSpanSettled(window: FleetHistoryWindow, toMs: number): boolean {
  return (window.newestMs !== null && window.newestMs >= toMs) || window.completeFromMs >= toMs;
}
