// Byte rates from two successive engine telemetry samples. Telemetry carries
// cumulative byte counters; a rate needs two samples of the same counter.

import type {
  EngineTelemetrySnapshot,
  TelemetryIngest,
} from "../core/api-types.js";

export interface TelemetryRates {
  /** Ingest bits per second, by pipeline id. */
  readonly ingestBps: ReadonlyMap<string, number>;
  /** Egress bits per second, by output id. */
  readonly egressBps: ReadonlyMap<string, number>;
}

function counterRate(
  previous: number | undefined,
  next: number | undefined,
  seconds: number,
): number | null {
  if (previous === undefined || next === undefined) return null;
  // A counter that went down belongs to a new session: no rate yet.
  if (next < previous) return null;
  return ((next - previous) * 8) / seconds;
}

/**
 * Rates between `previous` and `next`, or `null` when the samples cannot be
 * compared (missing, unordered or unparseable timestamps).
 */
export function telemetryRates(
  previous: EngineTelemetrySnapshot | null,
  next: EngineTelemetrySnapshot,
): TelemetryRates | null {
  if (previous === null) return null;
  const seconds =
    (Date.parse(next.generatedAt) - Date.parse(previous.generatedAt)) / 1000;
  if (!Number.isFinite(seconds) || seconds <= 0) return null;

  // An ingest is one publisher session of one pipeline; after a failover the
  // pipeline's counter belongs to another input and has no earlier sample.
  const ingestKey = (ingest: TelemetryIngest) =>
    `${ingest.pipelineId}\u0000${ingest.protocol}\u0000${ingest.streamKey ?? ""}`;
  const ingestBefore = new Map<string, number>();
  for (const ingest of previous.ingests) {
    if (ingest.pipelineId) ingestBefore.set(ingestKey(ingest), ingest.bytesReceived);
  }
  const ingestBps = new Map<string, number>();
  for (const ingest of next.ingests) {
    if (!ingest.pipelineId) continue;
    const rate = counterRate(
      ingestBefore.get(ingestKey(ingest)),
      ingest.bytesReceived,
      seconds,
    );
    if (rate !== null) ingestBps.set(ingest.pipelineId, rate);
  }

  const egressBefore = new Map<string, number | undefined>();
  for (const egress of previous.egresses) {
    egressBefore.set(egress.outputId, egress.bytesOut);
  }
  const egressBps = new Map<string, number>();
  for (const egress of next.egresses) {
    const rate = counterRate(
      egressBefore.get(egress.outputId),
      egress.bytesOut,
      seconds,
    );
    if (rate !== null) egressBps.set(egress.outputId, rate);
  }

  return { ingestBps, egressBps };
}
