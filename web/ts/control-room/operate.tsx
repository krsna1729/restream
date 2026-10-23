import { useCallback, useEffect, useMemo, useState } from "react";

import { useSession } from "./session.js";
import { SharePanel } from "./share-panel.js";
import { getConfig, promotePipelineInput } from "../core/api.js";
import { withBasePath } from "../core/base-path.js";
import type { ConfigData, PipelineInput } from "../types.js";
import { Kpi, KpiRow } from "../ui/kpi.js";
import { Panel } from "../ui/panel.js";
import { useEngineer } from "../ui/prefs.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import { DestinationMatrix } from "./destination-matrix.js";
import { formatBps } from "./format.js";
import { VARIANT_LABELS, inputSourceLines } from "./input-model.js";
import { AddInput, EditInput, InputActions, PipelineBar } from "./operate-manage.js";
import { MediaGraph } from "./operate-graph.js";
import { buildDestinationRows, buildMediaGraph, destinationFilterCounts } from "./operate-model.js";
import { PipelineScope } from "./pipeline-scope.js";
import { Freshness } from "./polled.js";
import { Preview } from "./preview.js";
import { pipelineGraph, pipelineInputs } from "./state-views.js";
import type { LiveSnapshot } from "./store.js";

/** The live pipeline cockpit: sources, the media path and destinations. */
export function Operate(props: { live: LiveSnapshot; pipelineId: string | null }) {
  return (
    <PipelineScope live={props.live} workspace="operate" pipelineId={props.pipelineId} verb="operate">
      {(pipelineId) => <Cockpit live={props.live} pipelineId={pipelineId} />}
    </PipelineScope>
  );
}

function Cockpit(props: { live: LiveSnapshot; pipelineId: string }) {
  const { pipelineId } = props;
  const { state, telemetry } = props.live;
  const outputs = state?.outputs ?? [];
  // Pushed state with the store's sampled counters: a change shows as soon
  // as the state stream carries it.
  const inputs = useMemo(() => pipelineInputs(state, telemetry, pipelineId), [state, telemetry, pipelineId]);
  const graph = useMemo(() => pipelineGraph(state, telemetry, pipelineId), [state, telemetry, pipelineId]);
  // Stored settings (URLs, output configs, profiles) are not in the state
  // stream: read once, and again after every edit.
  const [config, setConfig] = useState<ConfigData | null>(null);
  const session = useSession();
  const canOperate = session.can("operator");
  const isAdmin = session.can("admin");
  // A viewer may not read settings (stream keys); the views work without.
  const reloadConfig = useCallback(
    () => void (canOperate ? getConfig().then(setConfig) : setConfig(null)),
    [canOperate],
  );
  useEffect(reloadConfig, [reloadConfig]);
  const columns = useMemo(() => (graph ? buildMediaGraph(graph, outputs) : null), [graph, outputs]);
  const rows = useMemo(() => buildDestinationRows(pipelineId, outputs, graph), [pipelineId, outputs, graph]);
  const counts = useMemo(() => destinationFilterCounts(rows), [rows]);
  const stored = config?.pipelines.find((pipeline) => pipeline.id === pipelineId) ?? null;
  const pipeline = state?.pipelines.find((candidate) => candidate.id === pipelineId) ?? null;
  const egressBps = useMemo(() => {
    let total: number | null = null;
    for (const row of rows) {
      const rate = props.live.rates?.egressBps.get(row.output.id);
      if (rate !== undefined) total = (total ?? 0) + rate;
    }
    return total;
  }, [rows, props.live.rates]);
  const wanted = rows.filter((row) => row.output.desired === "running").length;
  return (
    <>
      <KpiRow max={5}>
        <Kpi
          label="Source"
          value={pipeline?.ingest ? pipeline.ingest.protocol.toUpperCase() : "offline"}
          tone={pipeline?.ingest ? "ok" : "bad"}
          foot={pipeline?.ingest ? `${formatBps(props.live.rates?.ingestBps.get(pipelineId))} · ${pipeline.standbyInputs} standby` : "no publisher connected"}
        />
        <Kpi
          label="Delivering"
          value={`${counts.delivering} / ${wanted}`}
          meter={wanted === 0 ? null : counts.delivering / wanted}
          meterTone={counts.attention > 0 ? "warn" : "ok"}
          foot="wanted destinations live"
        />
        <Kpi label="Egress" value={formatBps(egressBps)} foot={`${rows.length} destinations`} />
        <Kpi
          label="Needs attention"
          value={String(counts.attention)}
          tone={counts.attention > 0 ? "warn" : undefined}
          foot={`${counts.retrying} retrying · ${counts.starting} starting`}
        />
        <Kpi
          label="Recording"
          value={pipeline?.recording ? "recording" : "off"}
          tone={pipeline?.recording ? "bad" : undefined}
          foot={pipeline?.recording ? "a file is being written" : "not recording"}
        />
      </KpiRow>
      {stored && (
        <PipelineBar
          pipeline={stored}
          keyLocked={outputs.some((output) => output.pipelineId === pipelineId && output.desired === "running")}
          onChanged={reloadConfig}
        />
      )}
      <div className="grid gap-4 2xl:grid-cols-[20rem_minmax(0,1fr)]">
        <SourceDeck pipelineId={pipelineId} inputs={inputs} atMs={props.live.telemetryAtMs} />
        <MediaGraph
          pipelineId={pipelineId}
          columns={columns}
          atMs={props.live.telemetryAtMs}
          outputs={outputs}
          egresses={telemetry?.egresses ?? null}
          shards={props.live.metrics?.egressShards ?? null}
          shardsAtMs={props.live.metricsAtMs}
        />
      </div>
      <DestinationMatrix
        key={pipelineId}
        pipelineId={pipelineId}
        rows={rows}
        egressBps={props.live.rates?.egressBps ?? null}
        egresses={telemetry?.egresses ?? null}
        config={config}
        onConfigChanged={reloadConfig}
      />
      {isAdmin && <SharePanel pipelineId={pipelineId} pipelineName={pipeline?.name ?? pipelineId} />}
    </>
  );
}

function inputStatus(input: PipelineInput): { label: string; tone: Tone } {
  if (input.selected) {
    if (input.runtime.forwardingState === "awaitingKeyframe") return { label: "awaiting keyframe", tone: "info" };
    return input.runtime.connected ? { label: "on air", tone: "ok" } : { label: "selected · offline", tone: "bad" };
  }
  if (!input.enabled) return { label: "disabled", tone: "neutral" };
  return input.runtime.connected ? { label: "standby", tone: "info" } : { label: "offline", tone: "neutral" };
}

function SourceDeck(props: {
  pipelineId: string;
  inputs: readonly PipelineInput[] | null;
  /** When the counters (RTT) were sampled. */
  atMs: number | null;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const engineer = useEngineer();
  const { inputs } = props;
  const connected = inputs?.filter((input) => input.runtime.connected).length;
  return (
    <Panel
      id="operate-sources"
      title="Source deck"
      subtitle={inputs ? `${connected} / ${inputs.length} connected` : undefined}
      actions={<Freshness atMs={props.atMs} />}
    >
      {inputs === null ? (
        <p className="text-base-content/70">Loading inputs…</p>
      ) : (
        <ul className="grid gap-3 md:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-1">
          {inputs.map((input) => {
            const status = inputStatus(input);
            const video = input.runtime.video;
            const rtt = input.runtime.quality?.msRtt ?? input.runtime.quality?.tcpRttMs;
            const urls = inputSourceLines(input);
            return (
              <li
                key={input.id}
                aria-label={`Input ${input.label}`}
                className={`rounded-box border bg-base-100 p-3 text-sm ${input.selected && input.runtime.connected ? "border-success/40" : "border-base-content/10"}`}
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="min-w-0 truncate font-semibold">
                    {input.label} <span className="text-xs font-normal text-base-content/60">{input.role}</span>
                  </span>
                  <StatusBadge tone={status.tone} dot>
                    {status.label}
                  </StatusBadge>
                </div>
                {input.runtime.connected && (
                  <div className="mt-1 text-xs text-base-content/70 tabular">
                    {(input.runtime.protocol ?? "").toUpperCase()}
                    {video?.codec && ` · ${video.codec} ${video.width ?? "?"}×${video.height ?? "?"}`}
                    {video?.fps ? ` · ${Math.round(video.fps)} fps` : ""}
                    {typeof rtt === "number" && ` · RTT ${rtt.toFixed(0)} ms`}
                    {engineer && input.runtime.remoteAddr && ` · ${input.runtime.remoteAddr}`}
                  </div>
                )}
                {input.runtime.connected && <Preview url={withBasePath(input.previewUrl)} label={input.label} />}
                {input.enabled && !input.selected && (
                  <button
                    type="button"
                    className="btn btn-accent btn-sm mt-2 w-full"
                    disabled={busy !== null}
                    onClick={async () => {
                      setBusy(input.id);
                      try {
                        await promotePipelineInput(props.pipelineId, input.id);
                      } finally {
                        setBusy(null);
                      }
                    }}
                  >
                    {busy === input.id ? "Promoting…" : `Promote ${input.label} on air`}
                  </button>
                )}
                {input.kind === "hlsPull" ? (
                  <div className="mt-2 text-xs">
                    {urls.map((url) => (
                      <p key={url} className="font-mono break-all">
                        {url}
                      </p>
                    ))}
                    <p className="text-base-content/70">{VARIANT_LABELS[input.variantRule]}</p>
                  </div>
                ) : (
                  urls.length > 0 && (
                    <details className="mt-2 text-xs">
                      <summary className="cursor-pointer text-base-content/70">Publish URLs</summary>
                      {urls.map((url) => (
                        <p key={url} className="mt-1 font-mono break-all select-all">
                          {url}
                        </p>
                      ))}
                    </details>
                  )
                )}
                <InputActions pipelineId={props.pipelineId} input={input} />
                <EditInput pipelineId={props.pipelineId} input={input} />
              </li>
            );
          })}
        </ul>
      )}
      <div className="mt-3">
        <AddInput pipelineId={props.pipelineId} />
      </div>
    </Panel>
  );
}
