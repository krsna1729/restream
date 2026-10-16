import { useEffect, useMemo, useState } from "react";

import { listMediaFiles } from "../core/api.js";
import type { MediaFile } from "../core/api-types.js";

import {
  buildFleetModel,
  filterPipelineRows,
  OPERATIONAL,
  pipelineHealth,
} from "./fleet-model.js";
import type {
  FleetPipelineRow,
  HealthFilter,
  OperationalFilter,
  PipelineHealth,
} from "./fleet-model.js";
import { formatBps } from "./format.js";
import { PipelineForm } from "./pipeline-form.js";
import { protocolMix, sharedStages } from "./pipelines-model.js";
import type { ProtocolShare, SharedStages } from "./pipelines-model.js";
import { PlaceLink, useGo } from "./router.js";
import type { LiveSnapshot } from "./store.js";

const HEALTH_FILTERS: ReadonlyArray<[HealthFilter, string]> = [
  ["all", "All"],
  ["warning", "Degraded"],
  ["info", "Starting"],
  ["ok", "Healthy"],
  ["idle", "Idle"],
];

const OPERATIONAL_CARDS: ReadonlyArray<[OperationalFilter, string]> = [
  ["attention", "Needs attention"],
  ["standby", "Has warm standby"],
  ["bonded", "Uses bonded SRT"],
  ["recording", "Recording active"],
];

/** The pipeline inventory: every pipeline in one dense, filterable table. */
export function Pipelines({ live }: { live: LiveSnapshot }) {
  const [query, setQuery] = useState("");
  const [health, setHealth] = useState<HealthFilter>("all");
  const [operational, setOperational] = useState<OperationalFilter | null>(null);
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
  const sharing = useMemo(
    () => live.state && sharedStages(live.state.pipelines, live.state.outputs),
    [live.state],
  );
  const go = useGo();
  if (rows === null || sharing === null) {
    return <p className="opacity-70">Waiting for runtime state…</p>;
  }
  const shown = filterPipelineRows(rows, query, health, operational);
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
          keyLocked={false}
          onSaved={(pipeline) => go({ workspace: "operate", pipelineId: pipeline.id })}
        />
      )}
      <div className="grid gap-3 md:grid-cols-3">
        <ProtocolMix mix={protocolMix(rows)} />
        <StageSharing sharing={sharing} />
        <section aria-labelledby="operational-title" className="card card-compact bg-base-200">
          <div className="card-body">
            <h3 id="operational-title" className="card-title text-sm">
              Operational filters
            </h3>
            <div className="grid grid-cols-2 gap-2">
              {OPERATIONAL_CARDS.map(([value, label]) => {
                const count = rows.filter(OPERATIONAL[value]).length;
                return (
                  <button
                    key={value}
                    type="button"
                    aria-pressed={operational === value}
                    className={`btn btn-sm h-auto flex-col items-start py-2 ${
                      operational === value ? "btn-active" : ""
                    } ${value === "attention" && count > 0 ? "btn-warning btn-outline" : ""}`}
                    onClick={() => setOperational(operational === value ? null : value)}
                  >
                    <span>{label}</span>
                    <span className="text-xs font-normal opacity-70">
                      {count} {count === 1 ? "pipeline" : "pipelines"}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        </section>
      </div>
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

function ProtocolMix({ mix }: { mix: readonly ProtocolShare[] }) {
  const total = mix.reduce((sum, share) => sum + share.outputs, 0);
  return (
    <section aria-labelledby="protocol-mix-title" className="card card-compact bg-base-200">
      <div className="card-body">
        <h3 id="protocol-mix-title" className="card-title text-sm">
          Protocol mix
        </h3>
        {total === 0 ? (
          <p className="text-sm opacity-70">No outputs.</p>
        ) : (
          <ul className="space-y-1">
            {mix.map((share) => (
              <li key={share.protocol} className="grid grid-cols-[6rem_1fr_3rem] items-center gap-2 text-sm">
                <span>{share.protocol.toUpperCase()}</span>
                <progress
                  className="progress"
                  value={share.outputs}
                  max={total}
                  aria-label={`${share.protocol.toUpperCase()} share`}
                />
                <strong className="text-right tabular-nums">{share.outputs}</strong>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

function StageSharing({ sharing }: { sharing: SharedStages }) {
  const largest = sharing.groups[0];
  return (
    <section aria-labelledby="stage-sharing-title" className="card card-compact bg-base-200">
      <div className="card-body">
        <h3 id="stage-sharing-title" className="card-title text-sm">
          Shared-stage efficiency
        </h3>
        {largest ? (
          <>
            <div>
              <div className="text-xs opacity-70">Largest sharing group</div>
              <div className="text-lg font-semibold tabular-nums">
                {largest.outputs} outputs
              </div>
              <div className="text-xs opacity-70">
                one {largest.kind} stage · {largest.pipelineName}
              </div>
            </div>
            <p className="text-sm">
              {sharing.sharedOutputs} of {sharing.stagedOutputs} running outputs
              read a stage another output also reads ({sharing.groups.length}{" "}
              shared {sharing.groups.length === 1 ? "stage" : "stages"}).
            </p>
          </>
        ) : (
          <p className="text-sm opacity-70">
            {sharing.stagedOutputs === 0
              ? "No running output reads a stage."
              : `${sharing.stagedOutputs} running outputs, each on its own stage.`}
          </p>
        )}
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
