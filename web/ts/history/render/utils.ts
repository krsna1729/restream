import {
  escapeHtml,
  escapeRedactedHtml,
} from "../../core/utils.js";
import type { AppLogRow } from "../../types.js";

export interface HistoryEventClassification {
  type: string;
  label: string;
  badgeClass: string;
}

export interface PipelineIncident {
  badgeClass: string;
  correlationIds: string[];
  detailBadges: string[];
  endedAt: string | undefined;
  headline: string;
  logs: AppLogRow[];
  summary: string;
  startedAt: string | undefined;
}

export function formatHistoryTime(ts: string | null | undefined): string {
  if (!ts) return "--";
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return ts;
  return d.toLocaleString();
}

export function getNormalizedEventType(
  log: AppLogRow | null | undefined,
): string {
  return String(log?.eventType || "")
    .trim()
    .toLowerCase();
}

export function getEventData(
  log: AppLogRow | null | undefined,
): Record<string, unknown> | null {
  const fields = log?.fields;
  if (fields && typeof fields === "object")
    return fields as Record<string, unknown>;
  if (typeof fields !== "string" || !fields.trim()) return null;
  try {
    const parsed = JSON.parse(fields);
    return parsed && typeof parsed === "object"
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

export function getCorrelationId(
  log: AppLogRow | null | undefined,
): string | null {
  const data = getEventData(log);
  if (!data) return null;

  const rawValue =
    typeof data.correlation_id === "string"
      ? data.correlation_id
      : typeof data.correlationId === "string"
        ? data.correlationId
        : "";
  const correlationId = rawValue.trim();
  return correlationId || null;
}

export function formatCorrelationIdLabel(correlationId: string): string {
  const value = String(correlationId || "").trim();
  if (value.length <= 22) return value;
  return `${value.slice(0, 10)}...${value.slice(-8)}`;
}

export function renderCorrelationBadge(
  correlationId: string,
  sizeClass: "badge-xs" | "badge-sm" = "badge-sm",
): string {
  const full = escapeRedactedHtml(correlationId, true);
  const compact = escapeRedactedHtml(
    formatCorrelationIdLabel(correlationId),
    true,
  );
  return `<span class="badge ${sizeClass} badge-ghost" title="${full}">Corr ${compact}</span>`;
}

export function getDistinctCorrelationIds(logs: AppLogRow[]): string[] {
  return Array.from(
    new Set(
      (Array.isArray(logs) ? logs : [])
        .map((log) => getCorrelationId(log))
        .filter((value): value is string => Boolean(value)),
    ),
  );
}

export function getRawHistoryHaystack(log: AppLogRow): string {
  const fields =
    typeof log?.fields === "string"
      ? log.fields
      : log?.fields
        ? JSON.stringify(log.fields)
        : "";
  return `${log?.ts || ""}\n${log?.message || ""}\n${fields}`.toLowerCase();
}

export function inferIntentionalStop(logs: AppLogRow[], index: number): boolean {
  const entries = Array.isArray(logs) ? logs : [];
  const target = entries[index];
  if (!target) return false;

  const targetEventType = getNormalizedEventType(target);
  const targetEventData = getEventData(target);
  if (
    targetEventType === "lifecycle.exited" &&
    targetEventData?.requestedStop === true
  ) {
    return true;
  }

  const targetMessage = String(target.message || "");
  if (/requestedStop=true/.test(targetMessage)) return true;

  const windowStart = Math.max(0, index - 4);
  const windowEnd = Math.min(entries.length - 1, index + 6);
  for (let i = windowStart; i <= windowEnd; i += 1) {
    if (i === index) continue;
    const eventType = getNormalizedEventType(entries[i]);
    if (
      eventType === "lifecycle.stop_requested" ||
      eventType === "control.signal_requested"
    ) {
      return true;
    }
    const msg = String(entries[i]?.message || "");
    if (
      msg.startsWith("[lifecycle] stop_requested") ||
      msg.startsWith("[control] requested SIGTERM") ||
      /received signal 15/i.test(msg)
    ) {
      return true;
    }
  }

  return false;
}

export function getEventFieldString(
  log: AppLogRow,
  ...keys: string[]
): string | null {
  const data = getEventData(log);
  if (!data) return null;
  for (const key of keys) {
    const value = data[key];
    if (typeof value === "string" && value.trim()) {
      return value.trim();
    }
  }
  return null;
}

export function getPipelineStageEncoding(log: AppLogRow): string | null {
  const dataEncoding = getEventFieldString(
    log,
    "stage_encoding",
    "stageEncoding",
    "encoding",
  );
  if (dataEncoding) return dataEncoding;

  const message = String(log?.message || "");
  const extEncodingMatch = message.match(/\bencoding=([^\s)]+)/i);
  if (extEncodingMatch?.[1]) return extEncodingMatch[1].trim();

  const stderrEncodingMatch = message.match(/\([^:]+:([^)]+)\):/);
  if (stderrEncodingMatch?.[1]) return stderrEncodingMatch[1].trim();

  return null;
}

export function getPipelineStageBackend(log: AppLogRow): string | null {
  const backend = getEventFieldString(log, "stage_backend", "stageBackend");
  if (backend) {
    const normalized = backend.toLowerCase().replaceAll("-", "_");
    if (normalized === "external_ffmpeg") return "external_transcoder";
    return normalized;
  }
  return String(log?.target || "").includes("external_transcoder")
    ? "external_transcoder"
    : null;
}

export function getPipelineStageIdentity(log: AppLogRow): string | null {
  const pipelineId = String(log?.pipelineId || "").trim();
  const encoding = getPipelineStageEncoding(log);
  if (!pipelineId || !encoding) return null;
  const backend = getPipelineStageBackend(log) || "unknown";
  return `${pipelineId}::${backend}::${encoding}`;
}

export function getPipelineInputState(log: AppLogRow): string | null {
  const eventType = getNormalizedEventType(log);
  const eventData = getEventData(log);

  if (eventType === "pipeline.input_state.initialized") {
    const inputState = String(eventData?.state || "")
      .trim()
      .toLowerCase();
    return inputState || null;
  }
  if (eventType === "pipeline.input_state.transitioned") {
    const inputState = String(eventData?.to || "")
      .trim()
      .toLowerCase();
    return inputState || null;
  }
  if (eventType === "pipeline.input_state.reset") {
    return "reset";
  }

  const message = String(log?.message || "");
  if (!message.startsWith("[input_state]")) return null;
  if (message.includes("->")) {
    const inputState =
      message.split("->").pop()?.trim().toLowerCase() || "";
    return inputState || null;
  }
  const match = message.match(/initial_state\s*=\s*([a-z_]+)/i);
  return match?.[1]?.toLowerCase() || null;
}

export function distinctNonEmptyCount(
  values: Array<string | null | undefined>,
): number {
  return new Set(
    values
      .map((value) => String(value || "").trim())
      .filter((value) => value.length > 0),
  ).size;
}

export function getTargetAndLevel(log: AppLogRow): {
  target: string;
  level: string;
} {
  return {
    target: String(log?.target || ""),
    level: String(log?.level || "").toUpperCase(),
  };
}

export function getOrderedOutputLogs(
  logs: AppLogRow[],
  order: string,
): AppLogRow[] {
  const items = Array.isArray(logs) ? [...logs] : [];
  items.sort((a, b) => {
    const ta = Date.parse(a?.ts || "");
    const tb = Date.parse(b?.ts || "");
    const aMs = Number.isNaN(ta) ? 0 : ta;
    const bMs = Number.isNaN(tb) ? 0 : tb;
    return aMs - bMs;
  });
  return order === "asc" ? items : items.reverse();
}

export function parseHistoryTimeMs(ts: string | undefined): number | null {
  const value = Date.parse(ts || "");
  return Number.isNaN(value) ? null : value;
}

export function renderEventDataSummary(log: AppLogRow): string {
  const data = getEventData(log);
  if (!data) return "";
  const hiddenKeys = new Set([
    "correlation_id",
    "correlationId",
    "kind",
    "timestamp",
    "seq",
    "streamKey",
  ]);
  const entries = Object.entries(data)
    .filter(([key]) => !hiddenKeys.has(key))
    .slice(0, 5);
  if (entries.length === 0) return "";
  return `<div class="mt-2 flex flex-wrap gap-1">${entries
    .map(([key, value]) => {
      const rendered =
        value === null || value === undefined
          ? "--"
          : typeof value === "object"
            ? JSON.stringify(value)
            : String(value);
      return `<span class="border-base-content/10 bg-base-200/70 rounded-md border px-2 py-1 text-[11px]"><span class="text-base-content/50">${escapeHtml(key)}</span> <span class="font-mono">${escapeRedactedHtml(rendered, true)}</span></span>`;
    })
    .join("")}</div>`;
}
