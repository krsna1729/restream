import { useMemo, useState } from "react";

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
import { describeCause } from "./fleet-model.js";
import { formatBps } from "./format.js";
import { buildDestinationRows, buildMediaGraph } from "./operate-model.js";
import type { DestinationRow, GraphColumn } from "./operate-model.js";
import { PipelineScope } from "./pipeline-scope.js";
import { Freshness, usePolled } from "./polled.js";
import type { LiveSnapshot } from "./store.js";

/** The live pipeline cockpit: sources, the media path and destinations. */
export function Operate(props: { live: LiveSnapshot; pipelineId: string | null }) {
  return (
    <PipelineScope
      live={props.live}
      workspace="operate"
      pipelineId={props.pipelineId}
      verb="operate"
    >
      {(pipelineId) => <Cockpit live={props.live} pipelineId={pipelineId} />}
    </PipelineScope>
  );
}

function Cockpit(props: { live: LiveSnapshot; pipelineId: string }) {
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
    <>
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
    </>
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
