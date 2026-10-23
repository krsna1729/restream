import { useMemo, useState } from "react";

import { getResourceMap, getStageTelemetry } from "../core/api.js";
import type { GraphData, NodeJudgement, ResourceMapSnapshot } from "../core/api-types.js";
import { formatBytes, formatPercent } from "./format.js";
import {
  buildResourceRows,
  MAP_TOP_N,
  nodeCards,
  ringFromGraph,
  shardsFrom,
} from "./inspect-model.js";
import type { ResourceRow, RingView, ShardView } from "./inspect-model.js";
import { Diagnostics } from "./diagnostics.js";
import { PipelineScope } from "./pipeline-scope.js";
import { Freshness, usePolled } from "./polled.js";
import { pipelineGraph } from "./state-views.js";
import type { LiveSnapshot } from "./store.js";

const CONFIDENCE_BADGE: Record<string, string> = {
  measured: "badge-success",
  derived: "badge-info",
  estimated: "badge-warning",
};

/** The engineering view under the cockpit: who owns what, and how sure. */
export function Inspect(props: { live: LiveSnapshot; pipelineId: string | null }) {
  return (
    <PipelineScope
      live={props.live}
      workspace="inspect"
      pipelineId={props.pipelineId}
      verb="inspect"
    >
      {(pipelineId) => (
        <>
          <Anatomy live={props.live} pipelineId={pipelineId} />
          <Diagnostics
            pipeline={{
              id: pipelineId,
              name:
                props.live.state?.pipelines.find((pipeline) => pipeline.id === pipelineId)?.name ??
                pipelineId,
            }}
          />
        </>
      )}
    </PipelineScope>
  );
}

function Anatomy({ live, pipelineId }: { live: LiveSnapshot; pipelineId: string }) {
  // The pipeline's map, or every pipeline's (the runtime scope). The map is
  // process measurements (CPU, memory), so it is polled.
  const [global, setGlobal] = useState(false);
  const scope = global ? null : pipelineId;
  const map = usePolled<ResourceMapSnapshot>(["resource-map", scope, MAP_TOP_N], () =>
    getResourceMap(scope, { view: "detail", topN: MAP_TOP_N }),
  );
  const graph = useMemo(
    () => pipelineGraph(live.state, live.telemetry, pipelineId),
    [live.state, live.telemetry, pipelineId],
  );
  const rows = useMemo(
    () => (map.value ? buildResourceRows(map.value) : null),
    [map.value],
  );
  const truncated = map.value?.limits?.truncatedNodeCount ?? 0;
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const selected =
    rows?.find((row) => row.node.id === selectedId) ?? rows?.[0] ?? null;
  return (
    <>
      <div className="grid gap-4 lg:grid-cols-[1fr_22rem]">
        <section className="card bg-base-200" aria-labelledby="inspect-map">
          <div className="card-body gap-3 p-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <h3 id="inspect-map" className="card-title text-base">
                {global ? "Global resource map" : "Resource map"}
                <span className="text-xs font-normal text-base-content/70">
                  under pressure first, then by memory held
                </span>
              </h3>
              <span className="flex items-center gap-2">
                {truncated > 0 && (
                  <span className="badge badge-sm badge-warning">
                    {truncated} lighter nodes not shown
                  </span>
                )}
                <Freshness atMs={map.atMs} />
                <button
                  type="button"
                  className="btn btn-xs"
                  aria-pressed={global}
                  onClick={() => {
                    setGlobal(!global);
                    setSelectedId(null);
                  }}
                >
                  {global ? "This pipeline only" : "Global resource map"}
                </button>
              </span>
            </div>
            {rows === null ? (
              <p className="text-base-content/70">Loading resource map…</p>
            ) : (
              <ResourceTable
                rows={rows}
                selectedId={selected?.node.id ?? null}
                onSelect={setSelectedId}
              />
            )}
          </div>
        </section>
        {/* The graph is this pipeline's: it names consumers only here. */}
        <NodeDetail row={selected} graph={global ? null : graph} />
      </div>
      <div className="grid gap-4 lg:grid-cols-3">
        <RingPanel ring={graph ? ringFromGraph(graph) : null} atMs={live.telemetryAtMs} />
        <ShardsPanel
          shards={map.value ? shardsFrom(map.value) : null}
          truncated={truncated > 0}
        />
        <AttributionPanel attribution={map.value?.attribution ?? null} />
      </div>
    </>
  );
}

function ResourceTable(props: {
  rows: readonly ResourceRow[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}) {
  return (
    <div className="overflow-x-auto">
      <table className="table table-sm">
        <thead>
          <tr>
            <th>Node</th>
            <th>Kind</th>
            <th>Execution</th>
            <th>Memory</th>
            <th>Confidence</th>
            <th>Hotspot</th>
          </tr>
        </thead>
        <tbody>
          {props.rows.map(({ node, memoryBytes, confidence, hotspots }) => (
            <tr
              key={node.id}
              className={node.id === props.selectedId ? "bg-base-300" : undefined}
            >
              <td>
                <button
                  type="button"
                  className="link link-hover touch-36 text-left font-medium"
                  aria-pressed={node.id === props.selectedId}
                  onClick={() => props.onSelect(node.id)}
                >
                  {node.label}
                </button>
              </td>
              <td>{node.kind.replaceAll("_", " ")}</td>
              <td>{node.execution?.replaceAll("_", " ") ?? "—"}</td>
              <td className="tabular-nums">{formatBytes(memoryBytes)}</td>
              <td>
                {confidence && (
                  <span
                    className={`badge badge-sm ${CONFIDENCE_BADGE[confidence] ?? "badge-outline"}`}
                  >
                    {confidence}
                  </span>
                )}
              </td>
              <td>
                {hotspots.map((hotspot) => (
                  <span key={hotspot} className="badge badge-sm badge-warning mr-1">
                    {hotspot}
                  </span>
                ))}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function scalarEntries(
  record: Record<string, unknown> | null | undefined,
): Array<[string, string]> {
  return Object.entries(record ?? {})
    .filter(([, value]) => value !== null && typeof value !== "object")
    .map(([key, value]) => [
      key,
      typeof value === "number" && !Number.isInteger(value)
        ? value.toFixed(2)
        : String(value),
    ]);
}

function NodeDetail({ row, graph }: { row: ResourceRow | null; graph: GraphData | null }) {
  const cards = row ? nodeCards(row.node, graph) : [];
  return (
    <section className="card bg-base-200" aria-labelledby="inspect-node">
      <div className="card-body gap-3 p-4">
        <h3 id="inspect-node" className="card-title text-base">
          {row ? `Selected · ${row.node.label}` : "Selected node"}
        </h3>
        {cards.length > 0 && (
          <dl className="grid grid-cols-2 gap-2" aria-label="Node metrics">
            {cards.map((card) => (
              <div key={card.label} className="rounded bg-base-100 p-2">
                <dt className="text-xs text-base-content/70">{card.label}</dt>
                <dd className="text-lg font-semibold tabular-nums">{card.value}</dd>
                <dd className="text-xs text-base-content/60">{card.foot}</dd>
              </div>
            ))}
          </dl>
        )}
        {row?.node.judgement && <Judgement judgement={row.node.judgement} />}
        {row === null ? (
          <p className="text-base-content/70">No node.</p>
        ) : (
          <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
            {[
              ["id", row.node.id],
              ["kind", row.node.kind],
              ["execution", row.node.execution ?? "—"],
              ...(row.node.state ? [["state", row.node.state]] : []),
              ["memory", `${formatBytes(row.memoryBytes)} (${row.node.memory?.source ?? "—"})`],
              ...scalarEntries(row.node.threads).map(([k, v]) => [`threads.${k}`, v]),
              ...scalarEntries(row.node.metrics).map(([k, v]) => [k, v]),
            ].map(([key, value]) => (
              <div key={key} className="contents">
                <dt className="text-base-content/70">{key}</dt>
                <dd className="min-w-0 break-words tabular-nums">{value}</dd>
              </div>
            ))}
          </dl>
        )}
        {row?.node.kind === "stage" && <StagePipe key={row.node.id} stageKey={row.node.id} />}
      </div>
    </section>
  );
}

const VERDICT: Record<NodeJudgement["verdict"], { title: string; tone: string }> = {
  limiting: { title: "Limiting", tone: "alert-error" },
  pressured: { title: "Under pressure", tone: "alert-warning" },
  clear: { title: "Not a bottleneck", tone: "alert-success" },
};

/** The engine's judgement of the node against its service center. */
function Judgement({ judgement }: { judgement: NodeJudgement }) {
  const verdict = VERDICT[judgement.verdict];
  return (
    <div role="note" aria-label="Bottleneck judgement" className={`alert ${verdict.tone} alert-soft text-sm`}>
      <div>
        <b>{verdict.title}</b>
        <p>
          {judgement.reason}.{" "}
          <span className="text-base-content/70">
            {judgement.measured
              ? "From a measurement."
              : "From the capacity model's calibration priors (observe-only)."}
          </span>
        </p>
      </div>
    </div>
  );
}

/** The stage's pipe counters (stalls and idles writing to its ffmpeg),
 * which the resource map leaves out; fetched only for the selected stage. */
function StagePipe(props: { stageKey: string }) {
  // Every answer counts as a load, so a stage the engine no longer has
  // (null) is told apart from one not read yet.
  const stage = usePolled(["stage-telemetry", props.stageKey], async () => ({
    snapshot: await getStageTelemetry(props.stageKey),
  }));
  const snapshot = stage.value?.snapshot;
  const rows = scalarEntries(snapshot?.pipeMetrics);
  return (
    <div aria-label="Stage pipe" role="group" className="text-sm">
      <h4 className="font-medium">Pipe</h4>
      {stage.value === null ? (
        <p className="text-base-content/70">Loading stage detail…</p>
      ) : snapshot === null ? (
        <p className="text-base-content/70">No telemetry for this stage (it may have stopped).</p>
      ) : rows.length === 0 ? (
        <p className="text-base-content/70">This stage has no pipe.</p>
      ) : (
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
          {rows.map(([key, value]) => (
            <div key={key} className="contents">
              <dt className="text-base-content/70">{key}</dt>
              <dd className="min-w-0 break-words tabular-nums">{value}</dd>
            </div>
          ))}
        </dl>
      )}
    </div>
  );
}

function RingPanel(props: { ring: RingView | null; atMs: number | null }) {
  const { ring } = props;
  return (
    <section className="card bg-base-200" aria-labelledby="inspect-ring">
      <div className="card-body gap-2 p-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 id="inspect-ring" className="card-title text-base">
            Source ring
          </h3>
          <Freshness atMs={props.atMs} />
        </div>
        {ring === null ? (
          <p className="text-base-content/70">No source ring runs.</p>
        ) : (
          <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
            <dt className="text-base-content/70">Fill</dt>
            <dd className="tabular-nums">
              {ring.fill} / {ring.capacity} ·{" "}
              {formatPercent((100 * ring.fill) / ring.capacity)}
            </dd>
            <dt className="text-base-content/70">Retained payload</dt>
            <dd className="tabular-nums">{formatBytes(ring.payloadBytes)}</dd>
          </dl>
        )}
      </div>
    </section>
  );
}

function ShardsPanel(props: {
  shards: readonly ShardView[] | null;
  truncated: boolean;
}) {
  const { shards } = props;
  return (
    <section className="card bg-base-200" aria-labelledby="inspect-shards">
      <div className="card-body gap-2 p-4">
        <h3 id="inspect-shards" className="card-title text-base">
          Egress shards
        </h3>
        {props.truncated && (
          <p className="text-sm text-base-content/70">
            The map is truncated: some shards may not be listed.
          </p>
        )}
        {shards === null ? (
          <p className="text-base-content/70">Loading…</p>
        ) : shards.length === 0 ? (
          !props.truncated && (
            <p className="text-base-content/70">No output of this pipeline runs on a shard.</p>
          )
        ) : (
          <ul className="space-y-2">
            {shards.map((shard) => (
              <li
                key={shard.id}
                aria-label={`Shard ${shard.protocol} ${shard.index}`}
                className="rounded bg-base-100 p-2 text-sm"
              >
                <div className="flex items-center justify-between">
                  <span className="font-medium">
                    {shard.protocol.toUpperCase()} shard {shard.index}
                  </span>
                  <span
                    className={`badge badge-sm ${shard.state === "healthy" ? "badge-success" : "badge-warning"}`}
                  >
                    {shard.state}
                  </span>
                </div>
                <div className="break-all text-xs text-base-content/70">feed {shard.feed}</div>
                <div className="mt-1 text-base-content/70">
                  progress {shard.progressAgeMs ?? "--"} ms · commands{" "}
                  {shard.commandDepth ?? "--"} / {shard.commandCapacity ?? "--"} ·{" "}
                  {shard.mediaTicks ?? "--"} media ticks
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

function AttributionPanel(props: {
  attribution: Record<string, string[]> | null;
}) {
  return (
    <section className="card bg-base-200" aria-labelledby="inspect-attribution">
      <div className="card-body gap-2 p-4">
        <h3 id="inspect-attribution" className="card-title text-base">
          How sure the numbers are
        </h3>
        {props.attribution === null ? (
          <p className="text-base-content/70">Loading…</p>
        ) : (
          <dl className="space-y-1 text-sm">
            {Object.entries(props.attribution).map(([confidence, sources]) => (
              <div key={confidence}>
                <dt>
                  <span
                    className={`badge badge-sm ${CONFIDENCE_BADGE[confidence] ?? "badge-outline"}`}
                  >
                    {confidence}
                  </span>
                </dt>
                <dd className="text-base-content/70">{sources.join(", ").replaceAll("_", " ")}</dd>
              </div>
            ))}
          </dl>
        )}
      </div>
    </section>
  );
}
