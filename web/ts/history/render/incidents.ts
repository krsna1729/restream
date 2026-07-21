import type { AppLogRow } from "../../types.js";
import type { PipelineIncident } from "./utils.js";
import {
  getNormalizedEventType,
  getCorrelationId,
  getDistinctCorrelationIds,
  getPipelineStageIdentity,
  getPipelineInputState,
  getOrderedOutputLogs,
  parseHistoryTimeMs,
  distinctNonEmptyCount,
} from "./utils.js";
import { getPipelineSemanticKind } from "./classify.js";

export const PIPELINE_INCIDENT_WINDOW_MS = 20_000;
export const PIPELINE_INCIDENT_MAX_SPAN_MS = PIPELINE_INCIDENT_WINDOW_MS * 2;
export const PIPELINE_RELATION_SCORE_THRESHOLD = 45;

type PipelineIncidentLinkKind = "correlation" | "output" | "stage" | "causal";

interface PipelineIncidentRelation {
  kinds: Set<PipelineIncidentLinkKind>;
  score: number;
}

function isNearbyPipelineCausalPair(a: AppLogRow, b: AppLogRow): boolean {
  const aMs = parseHistoryTimeMs(a?.ts);
  const bMs = parseHistoryTimeMs(b?.ts);
  if (aMs === null || bMs === null) return false;
  if (Math.abs(aMs - bMs) > PIPELINE_INCIDENT_WINDOW_MS) return false;

  const [earlier, later] = aMs <= bMs ? [a, b] : [b, a];
  if (
    String(earlier?.pipelineId || "").trim() !==
    String(later?.pipelineId || "").trim()
  ) {
    return false;
  }

  const earlierKind = getPipelineSemanticKind(earlier);
  const laterKind = getPipelineSemanticKind(later);

  if (
    ["ingest_off", "input_warning", "input_error"].includes(earlierKind) &&
    ["stage_stop", "output_stop", "output_fail", "stage_fault"].includes(
      laterKind,
    )
  ) {
    return true;
  }
  if (
    earlierKind === "ingest_on" &&
    ["stage_start", "output_start"].includes(laterKind)
  ) {
    return true;
  }
  if (
    earlierKind === "config" &&
    [
      "stage_start",
      "stage_stop",
      "output_start",
      "output_stop",
      "output_fail",
      "stage_fault",
    ].includes(laterKind)
  ) {
    return true;
  }
  if (
    ["stage_fault", "stage_stop"].includes(earlierKind) &&
    ["output_fail", "output_stop"].includes(laterKind)
  ) {
    return true;
  }
  if (earlierKind === "stage_start" && laterKind === "output_start") {
    return true;
  }

  return false;
}

function getPipelineIncidentRelation(
  a: AppLogRow,
  b: AppLogRow,
): PipelineIncidentRelation {
  const kinds = new Set<PipelineIncidentLinkKind>();
  const aMs = parseHistoryTimeMs(a?.ts);
  const bMs = parseHistoryTimeMs(b?.ts);
  if (aMs === null || bMs === null) return { kinds, score: 0 };
  if (Math.abs(aMs - bMs) > PIPELINE_INCIDENT_WINDOW_MS) {
    return { kinds, score: 0 };
  }

  const pipelineA = String(a?.pipelineId || "").trim();
  const pipelineB = String(b?.pipelineId || "").trim();
  if (pipelineA && pipelineB && pipelineA !== pipelineB) {
    return { kinds, score: 0 };
  }

  let score = 0;
  const correlationA = getCorrelationId(a);
  const correlationB = getCorrelationId(b);
  if (correlationA && correlationA === correlationB) {
    kinds.add("correlation");
    score += 100;
  }

  const outputA = String(a?.outputId || "").trim();
  const outputB = String(b?.outputId || "").trim();
  if (outputA && outputA === outputB) {
    kinds.add("output");
    score += 70;
  }

  const stageA = getPipelineStageIdentity(a);
  const stageB = getPipelineStageIdentity(b);
  if (stageA && stageA === stageB) {
    kinds.add("stage");
    score += 60;
  }

  if (isNearbyPipelineCausalPair(a, b)) {
    kinds.add("causal");
    score += 50;
  }

  return { kinds, score };
}

function collectPipelineIncidentLinkKinds(
  logs: AppLogRow[],
): Set<PipelineIncidentLinkKind> {
  const linkKinds = new Set<PipelineIncidentLinkKind>();
  for (let i = 0; i < logs.length; i += 1) {
    for (let j = i + 1; j < logs.length; j += 1) {
      const relation = getPipelineIncidentRelation(logs[i], logs[j]);
      relation.kinds.forEach((kind) => linkKinds.add(kind));
    }
  }
  return linkKinds;
}

function splitPipelineIncidentCluster(logs: AppLogRow[]): AppLogRow[][] {
  const ordered = getOrderedOutputLogs(logs, "asc");
  if (ordered.length <= 1) return ordered.length > 0 ? [ordered] : [];

  const groups: AppLogRow[][] = [];
  let currentGroup: AppLogRow[] = [];
  let groupStartMs: number | null = null;

  ordered.forEach((log) => {
    const currentMs = parseHistoryTimeMs(log?.ts);
    if (currentGroup.length === 0) {
      currentGroup.push(log);
      groupStartMs = currentMs;
      return;
    }

    const withinSpan =
      currentMs !== null &&
      groupStartMs !== null &&
      currentMs - groupStartMs <= PIPELINE_INCIDENT_MAX_SPAN_MS;
    const hasStrongLink = currentGroup.some(
      (existing) =>
        getPipelineIncidentRelation(existing, log).score >=
        PIPELINE_RELATION_SCORE_THRESHOLD,
    );

    if (!withinSpan || !hasStrongLink) {
      groups.push(currentGroup);
      currentGroup = [log];
      groupStartMs = currentMs;
      return;
    }

    currentGroup.push(log);
  });

  if (currentGroup.length > 0) {
    groups.push(currentGroup);
  }

  return groups;
}

function summarizePipelineIncident(logs: AppLogRow[]): PipelineIncident {
  const entries = Array.isArray(logs) ? logs : [];
  const eventTypes = new Set(entries.map((log) => getNormalizedEventType(log)));
  const correlationIds = getDistinctCorrelationIds(entries);
  const linkKinds = collectPipelineIncidentLinkKinds(entries);
  const inputStates = new Set(
    entries
      .map((log) => getPipelineInputState(log))
      .filter((value): value is string => Boolean(value)),
  );
  const hasFfmpegFault = entries.some((log) => {
    const target = String(log?.target || "");
    const message = String(log?.message || "");
    const level = String(log?.level || "").toUpperCase();
    return (
      target.includes("external_transcoder") &&
      (level === "ERROR" ||
        level === "WARN" ||
        message.includes("failed to spawn ffmpeg") ||
        message.includes("stdin write failed") ||
        message.includes("ffmpeg stderr"))
    );
  });
  const hasConfigChange = entries.some((log) => {
    const eventType = getNormalizedEventType(log);
    const message = String(log?.message || "");
    return (
      eventType.startsWith("pipeline.config.") || message.startsWith("[config]")
    );
  });
  const hasIngestDisconnect =
    eventTypes.has("ingest.disconnected") || inputStates.has("off");
  const hasIngestConnect =
    eventTypes.has("ingest.connected") || inputStates.has("on");
  const outputFailureCount = distinctNonEmptyCount(
    entries
      .filter((log) => getNormalizedEventType(log) === "egress.failed")
      .map((log) => log.outputId),
  );
  const outputStopCount = distinctNonEmptyCount(
    entries
      .filter((log) => {
        const eventType = getNormalizedEventType(log);
        return eventType === "egress.stopped" || eventType === "lifecycle.stop";
      })
      .map((log) => log.outputId),
  );
  const outputStartCount = distinctNonEmptyCount(
    entries
      .filter((log) => {
        const eventType = getNormalizedEventType(log);
        return (
          eventType === "egress.started" || eventType === "lifecycle.start"
        );
      })
      .map((log) => log.outputId),
  );
  const stageStopCount = entries.filter(
    (log) => getNormalizedEventType(log) === "stage.stopped",
  ).length;
  const stageStartCount = entries.filter(
    (log) => getNormalizedEventType(log) === "stage.started",
  ).length;

  let headline = "Pipeline activity burst";
  let summary = `${entries.length} related pipeline events were recorded close together.`;
  let badgeClass = "badge-ghost";
  const detailBadges: string[] = [];

  if (
    hasIngestDisconnect &&
    (outputFailureCount > 0 || outputStopCount > 0 || stageStopCount > 0)
  ) {
    headline = "Input loss cascaded downstream";
    summary =
      "Publisher/input loss was followed by downstream output or stage changes.";
    badgeClass = outputFailureCount > 0 ? "badge-error" : "badge-warning";
    detailBadges.push("Cause: input disconnected");
  } else if (hasFfmpegFault && (outputFailureCount > 0 || stageStopCount > 0)) {
    headline = "External stage fault impacted outputs";
    summary =
      "The external FFmpeg stage emitted warnings or errors around the same time downstream behavior changed.";
    badgeClass = outputFailureCount > 0 ? "badge-error" : "badge-warning";
    detailBadges.push("Cause: external FFmpeg stage");
  } else if (outputFailureCount > 0) {
    headline = "Output delivery incident";
    summary = "One or more outputs failed during this activity burst.";
    badgeClass = "badge-error";
  } else if (
    hasIngestConnect &&
    (outputStartCount > 0 || stageStartCount > 0)
  ) {
    headline = "Pipeline came online";
    summary =
      "Publisher connectivity was followed by stage spin-up and output startup.";
    badgeClass = "badge-success";
    detailBadges.push("Cause: publisher connected");
  } else if (
    hasConfigChange &&
    (outputStartCount > 0 ||
      outputStopCount > 0 ||
      stageStartCount > 0 ||
      stageStopCount > 0)
  ) {
    headline = "Config change rolled through pipeline";
    summary =
      "A config update clustered with downstream stage or output lifecycle changes.";
    badgeClass = "badge-secondary";
    detailBadges.push("Context: config change");
  } else if (hasConfigChange) {
    headline = "Pipeline config changed";
    summary = "Configuration-related events were recorded for this pipeline.";
    badgeClass = "badge-secondary";
  } else if (inputStates.has("warning") || inputStates.has("error")) {
    headline = "Input health shifted";
    summary = "Input state changed into warning or error during this burst.";
    badgeClass = inputStates.has("error") ? "badge-error" : "badge-warning";
  } else if (outputStartCount > 0 || outputStopCount > 0) {
    headline = "Output lifecycle changed";
    summary =
      "Output start or stop activity clustered together in this window.";
    badgeClass = outputStopCount > 0 ? "badge-warning" : "badge-info";
  } else if (stageStartCount > 0 || stageStopCount > 0) {
    headline = "Stage lifecycle changed";
    summary =
      "Stage startup or shutdown events clustered together in this window.";
    badgeClass = stageStopCount > 0 ? "badge-warning" : "badge-info";
  }

  if (outputFailureCount > 0) {
    detailBadges.push(
      `Impact: ${outputFailureCount} output failure${outputFailureCount === 1 ? "" : "s"}`,
    );
  } else if (outputStopCount > 0) {
    detailBadges.push(
      `Impact: ${outputStopCount} output stop${outputStopCount === 1 ? "" : "s"}`,
    );
  } else if (outputStartCount > 0) {
    detailBadges.push(
      `Impact: ${outputStartCount} output start${outputStartCount === 1 ? "" : "s"}`,
    );
  }

  if (stageStopCount > 0) {
    detailBadges.push(`Stages: ${stageStopCount} stopped`);
  } else if (stageStartCount > 0) {
    detailBadges.push(`Stages: ${stageStartCount} started`);
  }

  if (linkKinds.has("correlation")) {
    detailBadges.push("Link: correlation id");
  } else if (linkKinds.has("output")) {
    detailBadges.push("Link: same output");
  } else if (linkKinds.has("stage")) {
    detailBadges.push("Link: same stage");
  } else if (linkKinds.has("causal")) {
    detailBadges.push("Link: nearby 20s");
  }

  return {
    badgeClass,
    correlationIds,
    detailBadges,
    endedAt: entries[entries.length - 1]?.ts,
    headline,
    logs: entries,
    summary,
    startedAt: entries[0]?.ts,
  };
}

export function buildPipelineIncidents(logs: AppLogRow[]): PipelineIncident[] {
  const entries = Array.isArray(logs) ? logs : [];
  if (entries.length === 0) return [];

  const ordered = getOrderedOutputLogs(entries, "asc");
  const parent = ordered.map((_, index) => index);

  const find = (index: number): number => {
    let root = index;
    while (parent[root] !== root) {
      root = parent[root];
    }
    while (parent[index] !== index) {
      const next = parent[index];
      parent[index] = root;
      index = next;
    }
    return root;
  };

  const union = (a: number, b: number): void => {
    const rootA = find(a);
    const rootB = find(b);
    if (rootA !== rootB) {
      parent[rootB] = rootA;
    }
  };

  for (let i = 0; i < ordered.length; i += 1) {
    const aMs = parseHistoryTimeMs(ordered[i]?.ts);
    if (aMs === null) continue;
    for (let j = i + 1; j < ordered.length; j += 1) {
      const bMs = parseHistoryTimeMs(ordered[j]?.ts);
      if (bMs === null) continue;
      if (bMs - aMs > PIPELINE_INCIDENT_WINDOW_MS) break;
      const relation = getPipelineIncidentRelation(ordered[i], ordered[j]);
      if (relation.score >= PIPELINE_RELATION_SCORE_THRESHOLD) {
        union(i, j);
      }
    }
  }

  const byRoot = new Map<number, AppLogRow[]>();
  ordered.forEach((log, index) => {
    const root = find(index);
    const group = byRoot.get(root);
    if (group) {
      group.push(log);
    } else {
      byRoot.set(root, [log]);
    }
  });

  return [...byRoot.values()]
    .flatMap((group) => splitPipelineIncidentCluster(group))
    .map(summarizePipelineIncident);
}
