import { useEffect, useMemo, useRef, useState } from "react";

import { getLogs } from "../core/api.js";
import { createManagedLogStream } from "../core/log-stream.js";
import type { AppLogRow } from "../types.js";
import { LOG_ROWS_KEPT, logFieldsText, logFilters, withLiveRow } from "./diagnose-model.js";
import type { LogLevel, LogScope } from "./diagnose-model.js";
import type { LiveSnapshot } from "./store.js";

const HISTORY_ROWS = 200;
const LEVELS: ReadonlyArray<[LogLevel, string]> = [
  ["error", "Errors"],
  ["warn", "Warnings and up"],
  ["info", "Info and up"],
  ["debug", "Debug and up"],
];
const LEVEL_BADGES: Record<string, string> = {
  ERROR: "badge-error",
  WARN: "badge-warning",
  INFO: "badge-ghost",
  DEBUG: "badge-ghost opacity-60",
};

/** The engine's log: the newest persisted rows for a scope, then every
 * matching row live as the engine writes it. */
export function Logs(props: { live: LiveSnapshot; pipelineId: string | null }) {
  const state = props.live.state;
  const [scope, setScope] = useState<LogScope>(
    props.pipelineId ? { kind: "pipeline", pipelineId: props.pipelineId, outputId: null } : { kind: "all" },
  );
  const [level, setLevel] = useState<LogLevel>("info");
  const [lifecycleOnly, setLifecycleOnly] = useState(false);
  const [following, setFollowing] = useState(true);
  const [rows, setRows] = useState<AppLogRow[] | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [streamDown, setStreamDown] = useState(false);
  /** Which filter set the history was read for, and how that read ended. */
  const [history, setHistory] = useState<{ key: string; status: "loaded" | "failed" } | null>(null);
  const [attempt, setAttempt] = useState(0);
  const shown = useRef(rows);
  shown.current = rows;
  const filters = useMemo(() => logFilters(scope, level, lifecycleOnly), [scope, level, lifecycleOnly]);
  const filterKey = JSON.stringify(filters);

  // The history is read once per filter set; Pause and Resume only close and
  // reopen the stream, so the rows on screen stay where they are.
  useEffect(() => {
    let alive = true;
    setRows(null);
    setHistory(null);
    void getLogs(filters, HISTORY_ROWS).then((page) => {
      if (!alive) return;
      if (page === null) {
        setHistory({ key: filterKey, status: "failed" });
        return;
      }
      setRows(page.logs);
      setHasMore(page.hasMore);
      setHistory({ key: filterKey, status: "loaded" });
    });
    return () => {
      alive = false;
    };
  }, [filterKey, attempt]);

  useEffect(() => {
    if (!following || history?.key !== filterKey || history.status !== "loaded") return;
    setStreamDown(false);
    const stream = createManagedLogStream();
    // Resume after the newest row shown: the engine sends what was written
    // since (what a pause missed, too), then live rows, so none is lost or
    // doubled. With nothing shown the stream starts live.
    const newest = Math.max(0, ...(shown.current ?? []).map((row) => row.id ?? 0));
    stream.sync({
      filters,
      resumeAfterId: newest || null,
      onLog: (row) => setRows((current) => withLiveRow(current ?? [], row)),
      onUnavailable: () => setStreamDown(true),
    });
    return () => stream.close();
  }, [filterKey, following, history]);

  const names = useMemo(() => {
    const map = new Map<string, string>();
    for (const pipeline of state?.pipelines ?? []) map.set(pipeline.id, pipeline.name);
    for (const output of state?.outputs ?? []) map.set(output.id, output.name);
    return map;
  }, [state]);
  const pipelineOutputs =
    scope.kind === "pipeline"
      ? (state?.outputs ?? []).filter((output) => output.pipelineId === scope.pipelineId)
      : [];

  return (
    <section className="space-y-4" aria-labelledby="logs-title">
      <div className="flex flex-wrap items-end gap-3">
        <h2 id="logs-title" className="text-xl font-semibold">
          Logs
        </h2>
        <label className="form-control">
          <span className="label-text text-xs">Scope</span>
          <select
            className="select select-sm select-bordered"
            value={scope.kind === "pipeline" ? `p:${scope.pipelineId}` : scope.kind}
            onChange={(event) => {
              const value = event.target.value;
              setScope(
                value.startsWith("p:")
                  ? { kind: "pipeline", pipelineId: value.slice(2), outputId: null }
                  : value === "engine"
                    ? { kind: "engine" }
                    : { kind: "all" },
              );
            }}
          >
            <option value="all">Everything</option>
            <option value="engine">Engine only (no pipeline)</option>
            {(state?.pipelines ?? []).map((pipeline) => (
              <option key={pipeline.id} value={`p:${pipeline.id}`}>
                Pipeline {pipeline.name}
              </option>
            ))}
          </select>
        </label>
        {scope.kind === "pipeline" && (
          <label className="form-control">
            <span className="label-text text-xs">Destination</span>
            <select
              className="select select-sm select-bordered"
              value={scope.outputId ?? ""}
              onChange={(event) => setScope({ ...scope, outputId: event.target.value || null })}
            >
              <option value="">All of the pipeline</option>
              {pipelineOutputs.map((output) => (
                <option key={output.id} value={output.id}>
                  {output.name}
                </option>
              ))}
            </select>
          </label>
        )}
        <label className="form-control">
          <span className="label-text text-xs">Level</span>
          <select
            className="select select-sm select-bordered"
            value={level}
            onChange={(event) => {
              const next = LEVELS.find(([value]) => value === event.target.value);
              if (next) setLevel(next[0]);
            }}
          >
            {LEVELS.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <label className="label cursor-pointer gap-2">
          <input
            type="checkbox"
            className="checkbox checkbox-sm"
            checked={lifecycleOnly}
            onChange={(event) => setLifecycleOnly(event.target.checked)}
          />
          <span className="label-text">Lifecycle events only</span>
        </label>
        <button
          type="button"
          className="btn btn-sm btn-ghost"
          aria-pressed={!following}
          onClick={() => setFollowing(!following)}
        >
          {following ? "Pause live" : "Resume live"}
        </button>
        <span role="status" className={`badge ${following && !streamDown ? "badge-success" : "badge-ghost"}`}>
          {!following ? "paused" : streamDown ? "live stream unavailable" : "live"}
        </span>
      </div>
      {history?.key === filterKey && history.status === "failed" ? (
        <p className="text-error">
          The engine did not return the log.{" "}
          <button type="button" className="btn btn-xs" onClick={() => setAttempt((count) => count + 1)}>
            Read again
          </button>
        </p>
      ) : rows === null ? (
        <p className="opacity-70">Loading log…</p>
      ) : rows.length === 0 ? (
        <p className="opacity-70">No rows match.</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="table table-xs" aria-label="Log rows">
            <thead>
              <tr>
                <th>Time</th>
                <th>Level</th>
                <th>Where</th>
                <th>Message</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row) => {
                const fields = logFieldsText(row);
                const where = row.outputId ?? row.pipelineId;
                return (
                  <tr key={row.id}>
                    <td className="whitespace-nowrap font-mono">{row.ts}</td>
                    <td>
                      <span className={`badge badge-sm ${LEVEL_BADGES[row.level ?? ""] ?? "badge-ghost"}`}>
                        {row.level}
                      </span>
                    </td>
                    <td className="whitespace-nowrap">{where ? (names.get(where) ?? where) : "engine"}</td>
                    <td className="font-mono">
                      {row.message}
                      {fields && (
                        <details>
                          <summary className="cursor-pointer opacity-70">fields</summary>
                          <pre className="whitespace-pre-wrap">{fields}</pre>
                        </details>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <p className="mt-2 text-xs opacity-70">
            Newest first; the page keeps {LOG_ROWS_KEPT} rows
            {hasMore && `, and the engine holds older rows than the first ${HISTORY_ROWS} read`}.
          </p>
        </div>
      )}
    </section>
  );
}
