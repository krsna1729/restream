import { useEffect, useMemo, useState } from "react";

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
  const filters = useMemo(() => logFilters(scope, level, lifecycleOnly), [scope, level, lifecycleOnly]);
  const filterKey = JSON.stringify(filters);

  useEffect(() => {
    let alive = true;
    const stream = createManagedLogStream();
    setRows(null);
    setStreamDown(false);
    void getLogs(filters, HISTORY_ROWS).then((page) => {
      if (!alive) return;
      const history = page?.logs ?? [];
      setRows(history);
      setHasMore(page?.hasMore ?? false);
      if (!following) return;
      // Resume after the newest row shown: the engine sends what was written
      // since the read, then live rows, so none is lost or doubled.
      const newest = Math.max(0, ...history.map((row) => row.id ?? 0));
      stream.sync({
        filters,
        resumeAfterId: newest || null,
        onLog: (row) => setRows((shown) => withLiveRow(shown ?? [], row)),
        onUnavailable: () => setStreamDown(true),
      });
    });
    return () => {
      alive = false;
      stream.close();
    };
  }, [filterKey, following]);

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
      {rows === null ? (
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
