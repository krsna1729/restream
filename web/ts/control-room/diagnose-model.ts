// Rules for the Logs workspace and the pipeline diagnostics run: which rows
// a scope asks the engine for, how live rows join the history, and the
// report a run downloads as.

import type { LogFilters } from "../core/api-types.js";
import type { AppLogRow, DiagnosticsReport } from "../types.js";

/** Whose rows: every row, engine-wide rows only, or one pipeline (with its
 * outputs' rows) or one of its outputs. */
export type LogScope =
  | { kind: "all" }
  | { kind: "engine" }
  | { kind: "pipeline"; pipelineId: string; outputId: string | null };

export type LogLevel = NonNullable<LogFilters["level"]>;

/** Rows a page keeps: the history page plus what streamed in since. */
export const LOG_ROWS_KEPT = 500;

/** The one filter set both the history read and the live stream use, so
 * the tail continues exactly the rows the history shows. */
export function logFilters(scope: LogScope, level: LogLevel, lifecycleOnly: boolean): LogFilters {
  const filters: LogFilters = { level };
  if (scope.kind === "engine") filters.scope = "restream";
  if (scope.kind === "pipeline") {
    filters.pipelineId = scope.pipelineId;
    if (scope.outputId) filters.outputId = scope.outputId;
  }
  if (lifecycleOnly) filters.eventClass = "lifecycle";
  return filters;
}

/** `rows` (newest first) with a streamed row on top; a row already shown
 * (the stream resumes at the history's newest id) is not shown twice. */
export function withLiveRow(rows: readonly AppLogRow[], row: AppLogRow): AppLogRow[] {
  if (rows.some((shown) => shown.id === row.id)) return [...rows];
  return [row, ...rows].slice(0, LOG_ROWS_KEPT);
}

/** A row's structured fields, indented; null when it has none. */
export function logFieldsText(row: AppLogRow): string | null {
  const { fields } = row;
  if (fields === null || fields === undefined || fields === "") return null;
  if (typeof fields !== "string") return JSON.stringify(fields, null, 2);
  try {
    return JSON.stringify(JSON.parse(fields), null, 2);
  } catch {
    return fields;
  }
}

/** How a run went: checks whose command failed, and checks that ran but
 * found issues. */
export function diagnosticsVerdict(report: DiagnosticsReport): {
  failed: number;
  withIssues: number;
} {
  return {
    failed: report.checks.filter((check) => check.exitCode !== 0).length,
    withIssues: report.checks.filter((check) => check.exitCode === 0 && check.issues.length > 0)
      .length,
  };
}

/** The downloadable report (`restream.diagnostics.v1`, as the classic
 * dashboard wrote it) and its file name. */
export function diagnosticsFile(
  pipeline: { id: string; name: string },
  report: DiagnosticsReport,
  generatedAt: Date,
): { name: string; json: string } {
  const stamp = generatedAt.toISOString().replace(/[:.]/g, "-").replace("T", "_").slice(0, 19);
  return {
    name: `diagnostic-${pipeline.name}-${pipeline.id}-${stamp}.json`.replace(/[^a-zA-Z0-9_.-]/g, "_"),
    json: JSON.stringify(
      {
        schema: "restream.diagnostics.v1",
        generatedAt: generatedAt.toISOString(),
        pipeline,
        probe: report.protocol,
        totalCommands: report.checks.length,
        totalDurationMs: report.totalDurationMs,
        results: report.checks,
      },
      null,
      2,
    ),
  };
}
