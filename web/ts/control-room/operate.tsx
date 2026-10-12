import { useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";

import {
  getPipelineInputs,
  getProcessingGraph,
  promotePipelineInput,
  startOut,
  stopOut,
} from "../core/api.js";
import type { GraphData } from "../core/api-types.js";
import type { OutputState } from "../core/generated/runtime-state.js";
import type { PipelineInput, PipelineInputsResponse } from "../types.js";
import { buildFleetModel, describeCause } from "./fleet-model.js";
import { formatAge, formatBps } from "./format.js";
import { buildDestinationRows, buildMediaGraph } from "./operate-model.js";
import type { DestinationRow, GraphColumn } from "./operate-model.js";
import { go, PlaceLink } from "./place-link.js";
import { HealthBadge } from "./pipelines.js";
import { SAMPLE_INTERVAL_MS, SAMPLE_STALE_AFTER_MS, useNow } from "./store.js";
import type { LiveSnapshot } from "./store.js";

interface Polled<T> {
  readonly value: T | null;
  /** Wall-clock ms of the last successful load. */
  readonly atMs: number | null;
  readonly refresh: () => void;
}

/**
 * `load` now and every sample interval while mounted; `refresh` loads now.
 * A failed load keeps the last value; `atMs` shows its age. The caller is
 * keyed by its subject, so a new subject remounts and starts from nothing:
 * one pipeline's data never shows under another's name.
 */
function usePolled<T>(load: () => Promise<T | null>): Polled<T> {
  const [loaded, setLoaded] = useState<{ value: T; atMs: number } | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    let alive = true;
    let timer: number | undefined;
    const loop = async () => {
      try {
        const next = await load();
        if (alive && next !== null) setLoaded({ value: next, atMs: Date.now() });
      } catch (error) {
        console.error("control room poll failed", error);
      } finally {
        if (alive) timer = window.setTimeout(loop, SAMPLE_INTERVAL_MS);
      }
    };
    void loop();
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [tick]);
  return {
    value: loaded?.value ?? null,
    atMs: loaded?.atMs ?? null,
    refresh: () => setTick((n) => n + 1),
  };
}

/** How old a polled panel's data is; amber once it missed two polls. */
function Freshness({ atMs }: { atMs: number | null }) {
  const now = useNow(1000);
  if (atMs === null) return null;
  const stale = now - atMs > SAMPLE_STALE_AFTER_MS;
  return (
    <span
      className={`badge badge-sm ${stale ? "badge-warning" : "badge-ghost"}`}
      title="time since the last successful update"
    >
      {stale ? "stale · " : ""}
      {formatAge(atMs, now)}
    </span>
  );
}

/** The live pipeline cockpit: sources, the media path and destinations. */
export function Operate({
  live,
  pipelineId,
}: {
  live: LiveSnapshot;
  pipelineId: string | null;
}) {
  const state = live.state;
  const row = useMemo(
    () =>
      state
        ? (buildFleetModel(state, live.rates).pipelines.find(
            (candidate) => candidate.id === pipelineId,
          ) ?? null)
        : null,
    [state, live.rates, pipelineId],
  );
  if (state === null) {
    return <p className="opacity-70">Waiting for runtime state…</p>;
  }
  if (pipelineId === null || row === null) {
    return (
      <section aria-labelledby="operate-pick" className="space-y-2">
        <h2 id="operate-pick" className="text-xl font-semibold">
          {pipelineId === null
            ? "Choose a pipeline to operate"
            : "That pipeline no longer exists"}
        </h2>
        <ul className="menu rounded-box bg-base-200">
          {state.pipelines.map((pipeline) => (
            <li key={pipeline.id}>
              <PlaceLink place={{ workspace: "operate", pipelineId: pipeline.id }}>
                {pipeline.name}
              </PlaceLink>
            </li>
          ))}
        </ul>
      </section>
    );
  }
  return (
    <Cockpit
      key={pipelineId}
      live={live}
      pipelineId={pipelineId}
      header={
        <div className="flex flex-wrap items-center gap-3">
          <h2 className="text-xl font-semibold">{row.name}</h2>
          <HealthBadge row={row} />
          <span className="text-sm opacity-70">
            {row.source
              ? `${row.source.protocol.toUpperCase()} in · ${formatBps(row.ingestBps)}`
              : "no input connected"}{" "}
            · {formatBps(row.egressBps)} out
          </span>
          <select
            className="select select-sm select-bordered ml-auto"
            aria-label="Pipeline"
            value={pipelineId}
            onChange={(event) =>
              go({ workspace: "operate", pipelineId: event.target.value })
            }
          >
            {state.pipelines.map((pipeline) => (
              <option key={pipeline.id} value={pipeline.id}>
                {pipeline.name}
              </option>
            ))}
          </select>
        </div>
      }
    />
  );
}

function Cockpit(props: {
  live: LiveSnapshot;
  pipelineId: string;
  header: ReactNode;
}) {
  const { pipelineId } = props;
  const outputs = props.live.state?.outputs ?? [];
  const inputs = usePolled<PipelineInputsResponse>(() =>
    getPipelineInputs(pipelineId),
  );
  const graph = usePolled<GraphData>(() => getProcessingGraph(pipelineId));
  const columns = useMemo(
    () => (graph.value ? buildMediaGraph(graph.value, outputs) : null),
    [graph.value, outputs],
  );
  const rows = useMemo(
    () => buildDestinationRows(pipelineId, outputs, graph.value),
    [pipelineId, outputs, graph.value],
  );
  return (
    <div className="space-y-4">
      {props.header}
      <div className="grid gap-4 lg:grid-cols-[18rem_1fr]">
        <SourceDeck
          pipelineId={pipelineId}
          inputs={inputs.value}
          atMs={inputs.atMs}
          onChanged={inputs.refresh}
        />
        <MediaGraph columns={columns} atMs={graph.atMs} />
      </div>
      <DestinationMatrix
        pipelineId={pipelineId}
        rows={rows}
        egressBps={props.live.rates?.egressBps ?? null}
      />
    </div>
  );
}

function inputStatus(input: PipelineInput): { label: string; badge: string } {
  if (input.selected) {
    if (input.runtime.forwardingState === "awaiting_keyframe") {
      return { label: "awaiting keyframe", badge: "badge-info" };
    }
    return input.runtime.connected
      ? { label: "active", badge: "badge-success" }
      : { label: "selected · offline", badge: "badge-warning" };
  }
  if (!input.enabled) return { label: "disabled", badge: "badge-ghost" };
  return input.runtime.connected
    ? { label: "standby", badge: "badge-info" }
    : { label: "offline", badge: "badge-ghost" };
}

function SourceDeck(props: {
  pipelineId: string;
  inputs: PipelineInputsResponse | null;
  atMs: number | null;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const inputs = props.inputs?.inputs ?? null;
  const connected = inputs?.filter((input) => input.runtime.connected).length;
  return (
    <section className="card bg-base-200" aria-labelledby="operate-sources">
      <div className="card-body gap-3 p-4">
        <div className="flex items-center justify-between gap-2">
          <h3 id="operate-sources" className="card-title text-base">
            Source deck
            {inputs && (
              <span className="text-xs font-normal opacity-70">
                {connected} / {inputs.length} connected
              </span>
            )}
          </h3>
          <Freshness atMs={props.atMs} />
        </div>
        {inputs === null ? (
          <p className="opacity-70">Loading inputs…</p>
        ) : (
          <ul className="space-y-2">
            {inputs.map((input) => {
              const status = inputStatus(input);
              const video = input.runtime.video;
              const rtt = input.runtime.quality?.msRtt;
              return (
                <li
                  key={input.id}
                  aria-label={`Input ${input.label}`}
                  className="rounded bg-base-100 p-2 text-sm"
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-medium">
                      {input.label}{" "}
                      <span className="text-xs opacity-60">{input.role}</span>
                    </span>
                    <span className={`badge badge-sm ${status.badge}`}>
                      {status.label}
                    </span>
                  </div>
                  {input.runtime.connected && (
                    <div className="mt-1 opacity-70">
                      {(input.runtime.protocol ?? "").toUpperCase()}
                      {video?.codec &&
                        ` · ${video.codec} ${video.width ?? "?"}×${video.height ?? "?"}`}
                      {typeof rtt === "number" && ` · RTT ${rtt.toFixed(0)} ms`}
                    </div>
                  )}
                  {input.enabled && !input.selected && (
                    <button
                      type="button"
                      className="btn btn-success btn-xs mt-2 w-full"
                      disabled={busy !== null}
                      onClick={async () => {
                        setBusy(input.id);
                        try {
                          await promotePipelineInput(props.pipelineId, input.id);
                        } finally {
                          setBusy(null);
                          props.onChanged();
                        }
                      }}
                    >
                      {busy === input.id ? "Promoting…" : `Promote ${input.label}`}
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </div>
    </section>
  );
}

function MediaGraph(props: {
  columns: readonly GraphColumn[] | null;
  atMs: number | null;
}) {
  const { columns } = props;
  return (
    <section className="card bg-base-200" aria-labelledby="operate-graph">
      <div className="card-body gap-3 p-4">
        <div className="flex items-center justify-between gap-2">
          <h3 id="operate-graph" className="card-title text-base">
            Live media graph
            <span className="text-xs font-normal opacity-70">
              shared work drawn once
            </span>
          </h3>
          <Freshness atMs={props.atMs} />
        </div>
        {columns === null ? (
          <p className="opacity-70">Loading graph…</p>
        ) : (
          <ol className="flex items-stretch gap-2 overflow-x-auto">
            {columns.map((column, index) => (
              <li key={index} className="flex items-center gap-2">
                {index > 0 && <span aria-hidden="true">→</span>}
                <ul className="flex flex-col gap-2">
                  {column.stages.map((stage) => (
                    <li
                      key={stage.id}
                      aria-label={`Stage ${stage.label}`}
                      className={`min-w-40 rounded border-l-4 bg-base-100 p-2 text-sm ${
                        stage.active ? "border-success" : "border-base-300"
                      }`}
                    >
                      <div className="text-xs uppercase opacity-60">
                        {stage.type.replace("_", " ")}
                      </div>
                      <div className="font-medium">{stage.label}</div>
                      <div className="text-xs opacity-70">
                        {stage.consumers} consumer{stage.consumers === 1 ? "" : "s"}
                        {stage.packetsPerSec !== null &&
                          ` · ${stage.packetsPerSec.toFixed(0)} pkt/s`}
                      </div>
                    </li>
                  ))}
                  {column.groups.map((group) => (
                    <li
                      key={group.key}
                      aria-label={`Destinations ${group.protocol ?? "unknown"}`}
                      className={`min-w-40 rounded border-l-4 bg-base-100 p-2 text-sm ${
                        group.attention > 0
                          ? "border-warning"
                          : group.delivering === group.outputs
                            ? "border-success"
                            : "border-base-300"
                      }`}
                    >
                      <div className="text-xs uppercase opacity-60">
                        egress group
                      </div>
                      <div className="font-medium">
                        {(group.protocol ?? "unknown").toUpperCase()}
                      </div>
                      <div className="text-xs opacity-70">
                        {group.delivering} / {group.outputs} delivering
                        {group.attention > 0 && ` · ${group.attention} degraded`}
                      </div>
                    </li>
                  ))}
                </ul>
              </li>
            ))}
          </ol>
        )}
      </div>
    </section>
  );
}

function evidence(output: OutputState): string {
  if (output.diagnosis) return describeCause(output.diagnosis.cause);
  if (output.evidence) return output.evidence.error;
  if (output.backpressure) return `held back: ${output.backpressure}`;
  return "";
}

function DestinationMatrix(props: {
  pipelineId: string;
  rows: readonly DestinationRow[];
  /** Per output, from two telemetry samples (the header's source too). */
  egressBps: ReadonlyMap<string, number> | null;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  return (
    <section aria-labelledby="operate-destinations">
      <h3 id="operate-destinations" className="mb-2 font-semibold">
        Destination matrix{" "}
        <span className="text-xs font-normal opacity-70">
          degraded first · {props.rows.length} outputs
        </span>
      </h3>
      <div className="overflow-x-auto">
        <table className="table table-sm">
          <thead>
            <tr>
              <th>Destination</th>
              <th>Protocol</th>
              <th>Desired</th>
              <th>Actual</th>
              <th>Convergence</th>
              <th>Rate</th>
              <th>Feed lag</th>
              <th>Shard</th>
              <th>Evidence</th>
              <th>
                <span className="sr-only">Action</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {props.rows.map(({ output, delivering, feedLagUnits }) => {
              const running = output.desired === "running";
              return (
                <tr key={output.id}>
                  <td className="font-medium">{output.name}</td>
                  <td>{output.protocol.toUpperCase()}</td>
                  <td>{output.desired}</td>
                  <td>{output.actual ?? "—"}</td>
                  <td>
                    <span
                      className={`badge badge-sm ${
                        output.diagnosis
                          ? "badge-warning"
                          : delivering
                            ? "badge-success"
                            : "badge-ghost"
                      }`}
                    >
                      {output.convergence} · {output.flow}
                    </span>
                    {output.retry && (
                      <span className="ml-1 text-xs opacity-70">
                        attempt {output.retry.attempts}
                      </span>
                    )}
                  </td>
                  <td className="tabular-nums">
                    {formatBps(props.egressBps?.get(output.id))}
                  </td>
                  <td className="tabular-nums">{feedLagUnits ?? "—"}</td>
                  <td className="tabular-nums">{output.shard ?? "—"}</td>
                  <td className="max-w-xs truncate" title={evidence(output)}>
                    {evidence(output)}
                  </td>
                  <td>
                    <button
                      type="button"
                      className={`btn btn-xs ${running ? "btn-outline" : "btn-primary"}`}
                      disabled={busy !== null}
                      aria-label={`${running ? "Stop" : "Start"} ${output.name}`}
                      onClick={async () => {
                        setBusy(output.id);
                        try {
                          await (running ? stopOut : startOut)(
                            props.pipelineId,
                            output.id,
                          );
                        } finally {
                          setBusy(null);
                        }
                      }}
                    >
                      {busy === output.id ? "…" : running ? "Stop" : "Start"}
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}
