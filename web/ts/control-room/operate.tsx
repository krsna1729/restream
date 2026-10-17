import { useCallback, useEffect, useMemo, useState } from "react";

import {
  deleteOutput,
  getConfig,
  getPipelineInputs,
  getProcessingGraph,
  getYoutubeMonitoringStatus,
  promotePipelineInput,
  startOut,
  stopOut,
} from "../core/api.js";
import { withBasePath } from "../core/base-path.js";
import type { GraphData } from "../core/api-types.js";
import type { OutputState } from "../core/generated/runtime-state.js";
import type { ConfigData, PipelineInput, PipelineInputsResponse } from "../types.js";
import { ConfirmDelete } from "./confirm.js";
import { describeCause } from "./fleet-model.js";
import { formatBps } from "./format.js";
import { VARIANT_LABELS, inputSourceLines } from "./input-model.js";
import { AddInput, EditInput, InputActions, PipelineBar } from "./operate-manage.js";
import { MediaGraph } from "./operate-graph.js";
import {
  buildDestinationRows,
  buildMediaGraph,
  DESTINATION_ROW_LIMIT,
  isYoutubeUrl,
  windowDestinationRows,
  youtubeVerdict,
} from "./operate-model.js";
import type { DestinationRow } from "./operate-model.js";
import { OutputForm } from "./output-form.js";
import { PipelineScope } from "./pipeline-scope.js";
import { Freshness, usePolled } from "./polled.js";
import { Preview } from "./preview.js";
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
  const inputs = usePolled<PipelineInputsResponse>(
    ["pipeline-inputs", pipelineId],
    () => getPipelineInputs(pipelineId),
  );
  const graph = usePolled<GraphData>(["processing-graph", pipelineId], () =>
    getProcessingGraph(pipelineId),
  );
  // Stored settings (URLs, output configs, profiles) are not in the state
  // stream: read once, and again after every edit.
  const [config, setConfig] = useState<ConfigData | null>(null);
  const reloadConfig = useCallback(() => void getConfig().then(setConfig), []);
  useEffect(reloadConfig, [reloadConfig]);
  const columns = useMemo(
    () => (graph.value ? buildMediaGraph(graph.value, outputs) : null),
    [graph.value, outputs],
  );
  const rows = useMemo(
    () => buildDestinationRows(pipelineId, outputs, graph.value),
    [pipelineId, outputs, graph.value],
  );
  const stored = config?.pipelines.find((pipeline) => pipeline.id === pipelineId) ?? null;
  return (
    <>
      {stored && (
        <PipelineBar
          pipeline={stored}
          keyLocked={outputs.some(
            (output) => output.pipelineId === pipelineId && output.desired === "running",
          )}
          onChanged={reloadConfig}
        />
      )}
      <div className="grid gap-4 lg:grid-cols-[18rem_1fr]">
        <SourceDeck
          pipelineId={pipelineId}
          inputs={inputs.value}
          atMs={inputs.atMs}
          onChanged={inputs.refresh}
        />
        <MediaGraph
          pipelineId={pipelineId}
          columns={columns}
          atMs={graph.atMs}
          outputs={outputs}
          egresses={props.live.telemetry?.egresses ?? null}
          shards={props.live.metrics?.egressShards ?? null}
          shardsAtMs={props.live.metricsAtMs}
        />
      </div>
      <DestinationMatrix
        key={pipelineId}
        pipelineId={pipelineId}
        rows={rows}
        egressBps={props.live.rates?.egressBps ?? null}
        config={config}
        onConfigChanged={reloadConfig}
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
              const urls = inputSourceLines(input);
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
                  {input.runtime.connected && (
                    <Preview url={withBasePath(input.previewUrl)} label={input.label} />
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
                  {input.kind === "hlsPull" ? (
                    <div className="mt-1 text-xs">
                      {urls.map((url) => (
                        <p key={url} className="break-all font-mono">
                          {url}
                        </p>
                      ))}
                      <p className="opacity-70">
                        {VARIANT_LABELS[input.variantRule]} ·{" "}
                        <a className="link" href={withBasePath(input.previewUrl)}>
                          Preview
                        </a>
                      </p>
                    </div>
                  ) : (
                    urls.length > 0 && (
                      <details className="mt-1 text-xs">
                        <summary className="cursor-pointer opacity-70">Publish URLs</summary>
                        {urls.map((url) => (
                          <p key={url} className="break-all font-mono">
                            {url}
                          </p>
                        ))}
                      </details>
                    )
                  )}
                  <InputActions
                    pipelineId={props.pipelineId}
                    input={input}
                    onChanged={props.onChanged}
                  />
                  <EditInput
                    pipelineId={props.pipelineId}
                    input={input}
                    onChanged={props.onChanged}
                  />
                </li>
              );
            })}
          </ul>
        )}
        <AddInput pipelineId={props.pipelineId} onChanged={props.onChanged} />
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
  /** Stored outputs (URL, config) and profiles, for the editor. */
  config: ConfigData | null;
  onConfigChanged: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  /** The output id being edited, "new", or null. */
  const [editing, setEditing] = useState<string | null>(null);
  /** The cap is lifted only when the operator asks for the whole fleet. */
  const [showAll, setShowAll] = useState(false);
  const { config } = props;
  /** Stored monitoring URLs by output id: one pass per config, not one
   * `find` per matrix row. */
  const monitoringUrls = useMemo(
    () =>
      new Map(
        (config?.outputs ?? []).map((stored) => [
          stored.id,
          stored.monitoringUrl ?? "",
        ]),
      ),
    [config],
  );
  const { visible, hidden, hiddenDegraded } = useMemo(
    () =>
      windowDestinationRows(
        props.rows,
        showAll ? Number.POSITIVE_INFINITY : DESTINATION_ROW_LIMIT,
      ),
    [props.rows, showAll],
  );
  const editedOutput =
    editing && editing !== "new"
      ? (config?.outputs.find((output) => output.id === editing) ?? null)
      : null;
  return (
    <section aria-labelledby="operate-destinations" className="space-y-3">
      <div className="flex items-center gap-3">
        <h3 id="operate-destinations" className="font-semibold">
          Destination matrix{" "}
          <span className="text-xs font-normal opacity-70">
            degraded first · {props.rows.length} outputs
          </span>
        </h3>
        <button
          type="button"
          className="btn btn-xs btn-primary"
          disabled={config === null}
          onClick={() => setEditing("new")}
        >
          Add destination
        </button>
      </div>
      {config && (editing === "new" || editedOutput) && (
        <OutputForm
          key={editing}
          pipelineId={props.pipelineId}
          stored={editedOutput}
          profiles={Object.keys(config.transcodeProfiles ?? {})}
          defaultHost={config.ingestHost || "localhost"}
          onSaved={(output) => {
            // A new destination continues as an edit of what was stored.
            setEditing(output.id);
            props.onConfigChanged();
          }}
          onClose={() => setEditing(null)}
        />
      )}
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
            {visible.map(({ output, delivering, feedLagUnits }) => {
              const running = output.desired === "running";
              const monitoringUrl = monitoringUrls.get(output.id) ?? "";
              return (
                <tr key={output.id}>
                  <td className="font-medium">
                    {output.name}
                    {isYoutubeUrl(monitoringUrl) && (
                      <YoutubeCheck name={output.name} url={monitoringUrl} />
                    )}
                  </td>
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
                    <button
                      type="button"
                      className="btn btn-xs btn-ghost ml-1"
                      aria-label={`Edit ${output.name}`}
                      disabled={config === null}
                      onClick={() => setEditing(output.id)}
                    >
                      Edit
                    </button>
                    <span className="ml-1">
                      <ConfirmDelete
                        label={`Delete ${output.name}`}
                        question="Delete this destination?"
                        onDelete={async () => {
                          const deleted =
                            (await deleteOutput(props.pipelineId, output.id)) !== null;
                          if (deleted) {
                            if (editing === output.id) setEditing(null);
                            props.onConfigChanged();
                          }
                          return deleted;
                        }}
                      />
                    </span>
                  </td>
                </tr>
              );
            })}
            {hidden > 0 && (
              <tr>
                <td colSpan={10} className="text-xs opacity-70">
                  {hidden} more not shown
                  {hiddenDegraded > 0 && ` (${hiddenDegraded} need attention)`}{" "}
                  <button
                    type="button"
                    className="link link-primary"
                    onClick={() => setShowAll(true)}
                  >
                    Show all {props.rows.length}
                  </button>
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </section>
  );
}

/** On request, asks the engine what YouTube's watch page says about the
 * stream; never polled. */
function YoutubeCheck(props: { name: string; url: string }) {
  const [checking, setChecking] = useState(false);
  const [verdict, setVerdict] = useState<{ text: string; title: string } | null>(null);
  return (
    <span className="ml-2 text-xs font-normal">
      <button
        type="button"
        className="btn btn-xs btn-ghost"
        disabled={checking}
        aria-label={`Check ${props.name} on YouTube`}
        onClick={async () => {
          setChecking(true);
          try {
            const status = await getYoutubeMonitoringStatus(props.url);
            setVerdict({ text: youtubeVerdict(status), title: status?.title ?? "" });
          } finally {
            setChecking(false);
          }
        }}
      >
        {checking ? "…" : "YouTube"}
      </button>
      {verdict && (
        <span role="status" title={verdict.title}>
          {verdict.text}
        </span>
      )}
    </span>
  );
}
