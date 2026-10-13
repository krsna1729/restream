import { useEffect, useMemo, useState } from "react";

import { listMediaFiles } from "../core/api.js";
import type { MediaFile } from "../core/api-types.js";

import {
  buildFleetModel,
  filterPipelineRows,
  pipelineHealth,
} from "./fleet-model.js";
import type {
  FleetPipelineRow,
  HealthFilter,
  PipelineHealth,
} from "./fleet-model.js";
import { formatBps } from "./format.js";
import { PipelineForm } from "./pipeline-form.js";
import { PlaceLink, go } from "./place-link.js";
import type { LiveSnapshot } from "./store.js";

const HEALTH_FILTERS: ReadonlyArray<[HealthFilter, string]> = [
  ["all", "All"],
  ["warning", "Degraded"],
  ["info", "Starting"],
  ["ok", "Healthy"],
  ["idle", "Idle"],
];

/** The pipeline inventory: every pipeline in one dense, filterable table. */
export function Pipelines({ live }: { live: LiveSnapshot }) {
  const [query, setQuery] = useState("");
  const [health, setHealth] = useState<HealthFilter>("all");
  const [creating, setCreating] = useState(false);
  const [files, setFiles] = useState<readonly MediaFile[]>([]);
  useEffect(() => {
    if (creating) void listMediaFiles().then((media) => setFiles(media?.files ?? []));
  }, [creating]);
  const rows = useMemo(
    () =>
      live.state ? buildFleetModel(live.state, live.rates).pipelines : null,
    [live.state, live.rates],
  );
  if (rows === null) {
    return <p className="opacity-70">Waiting for runtime state…</p>;
  }
  const shown = filterPipelineRows(rows, query, health);
  return (
    <section aria-labelledby="pipelines-title" className="space-y-3">
      <div className="flex flex-wrap items-end gap-3">
        <h2 id="pipelines-title" className="mr-auto text-xl font-semibold">
          Pipelines{" "}
          <span className="text-sm font-normal opacity-70">
            {shown.length} of {rows.length} · attention first, then size
          </span>
        </h2>
        <input
          type="search"
          className="input input-sm input-bordered"
          placeholder="Name or id"
          aria-label="Search pipelines"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
        <div role="group" aria-label="Health" className="join">
          {HEALTH_FILTERS.map(([value, label]) => (
            <button
              key={value}
              type="button"
              aria-pressed={health === value}
              className={`btn join-item btn-sm ${health === value ? "btn-active" : ""}`}
              onClick={() => setHealth(value)}
            >
              {label}
            </button>
          ))}
        </div>
        <button
          type="button"
          className="btn btn-sm btn-primary"
          aria-expanded={creating}
          onClick={() => setCreating(!creating)}
        >
          New pipeline
        </button>
      </div>
      {creating && (
        <PipelineForm
          stored={null}
          files={files}
          onSaved={(pipeline) => go({ workspace: "operate", pipelineId: pipeline.id })}
        />
      )}
      <div className="overflow-x-auto">
        <table className="table table-sm">
          <thead>
            <tr>
              <th>Pipeline</th>
              <th>Source</th>
              <th>Delivering</th>
              <th>Outputs by protocol</th>
              <th>Flow</th>
              <th>Health</th>
            </tr>
          </thead>
          <tbody>
            {shown.map((row) => (
              <tr key={row.id}>
                <td>
                  <PlaceLink
                    className="link font-medium"
                    place={{ workspace: "operate", pipelineId: row.id }}
                  >
                    {row.name}
                  </PlaceLink>
                  <div className="text-xs opacity-60">{row.id}</div>
                </td>
                <td>
                  {row.source
                    ? `${row.source.protocol.toUpperCase()}${
                        row.source.videoCodec ? ` · ${row.source.videoCodec}` : ""
                      }`
                    : "no input"}
                </td>
                <td className="tabular-nums">
                  {row.delivering} / {row.wanted}
                </td>
                <td>
                  {[...row.protocols]
                    .map(([protocol, count]) => `${protocol.toUpperCase()} ${count}`)
                    .join(" · ") || "none"}
                </td>
                <td className="tabular-nums">
                  {formatBps(row.ingestBps)} → {formatBps(row.egressBps)}
                </td>
                <td>
                  <HealthBadge row={row} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

const HEALTH_BADGE: Record<PipelineHealth["tone"], string> = {
  warning: "badge-warning",
  info: "badge-info",
  idle: "badge-ghost",
  ok: "badge-success",
};

export function HealthBadge({ row }: { row: FleetPipelineRow }) {
  const health = pipelineHealth(row);
  return (
    <span className={`badge badge-sm ${HEALTH_BADGE[health.tone]}`}>
      {health.label}
    </span>
  );
}
