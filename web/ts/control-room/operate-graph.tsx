import { useMemo, useState } from "react";

import { programPreviewUrl } from "../core/api.js";
import type { TelemetryEgress } from "../core/api-types.js";
import type { OutputState } from "../core/generated/runtime-state.js";
import type { EgressShardStatus } from "../types.js";
import { inspectGroup } from "./operate-group-model.js";
import type { EgressGroup, GraphColumn } from "./operate-model.js";
import { MoqPreview, moqPreviewSupported } from "./moq-preview.js";
import { Freshness, useLifecycleEvents } from "./polled.js";
import { Preview } from "./preview.js";

type Tab = "graph" | "destinations" | "timeline" | "preview";

const TABS: ReadonlyArray<[Tab, string]> = [
  ["graph", "Graph"],
  ["destinations", "Destinations"],
  ["timeline", "Timeline"],
  ["preview", "Preview"],
];

function groupBorder(group: EgressGroup): string {
  if (group.attention > 0) return "border-warning";
  return group.delivering === group.outputs ? "border-success" : "border-base-300";
}

function groupName(group: EgressGroup): string {
  return (group.protocol ?? "unknown").toUpperCase();
}

/** The pipeline's media path in four views, and the egress group the
 * operator selected in the Graph or Destinations view. */
export function MediaGraph(props: {
  pipelineId: string;
  columns: readonly GraphColumn[] | null;
  atMs: number | null;
  outputs: readonly OutputState[];
  egresses: readonly TelemetryEgress[] | null;
  /** The live store's sampled shard statuses and when they were read. */
  shards: readonly EgressShardStatus[] | null;
  shardsAtMs: number | null;
}) {
  const [tab, setTab] = useState<Tab>("graph");
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const groups = useMemo(
    () => (props.columns ?? []).flatMap((column) => column.groups),
    [props.columns],
  );
  const selected = groups.find((group) => group.key === selectedKey) ?? null;
  const select = (group: EgressGroup) =>
    setSelectedKey(group.key === selectedKey ? null : group.key);
  return (
    <div className="space-y-4">
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
          <div role="tablist" aria-label="Media path views" className="tabs tabs-box tabs-sm">
            {TABS.map(([value, label]) => (
              <button
                key={value}
                type="button"
                role="tab"
                aria-selected={tab === value}
                className={`tab ${tab === value ? "tab-active" : ""}`}
                onClick={() => setTab(value)}
              >
                {label}
              </button>
            ))}
          </div>
          <div role="tabpanel" aria-label={TABS.find(([value]) => value === tab)?.[1]}>
            {tab === "graph" && (
              <GraphView columns={props.columns} selected={selectedKey} onSelect={select} />
            )}
            {tab === "destinations" && (
              <GroupTable groups={props.columns === null ? null : groups} selected={selectedKey} onSelect={select} />
            )}
            {tab === "timeline" && (
              <Timeline pipelineId={props.pipelineId} outputs={props.outputs} />
            )}
            {tab === "preview" && <ProgramPreview pipelineId={props.pipelineId} />}
          </div>
        </div>
      </section>
      {selected && (
        <GroupInspector
          group={selected}
          outputs={props.outputs}
          egresses={props.egresses}
          shards={props.shards}
          shardsAtMs={props.shardsAtMs}
        />
      )}
    </div>
  );
}

type PreviewTransport = "hls" | "moq";
const PREVIEW_TRANSPORT_KEY = "restream.previewTransport";

function storedTransport(): PreviewTransport {
  try {
    return localStorage.getItem(PREVIEW_TRANSPORT_KEY) === "moq" ? "moq" : "hls";
  } catch {
    return "hls";
  }
}

/** The program preview over HLS (default, plays everywhere) or MoQ (lower
 * latency; needs WebTransport and WebCodecs and the engine's MoQ listener).
 * The operator's choice is kept across visits. */
function ProgramPreview(props: { pipelineId: string }) {
  const supported = moqPreviewSupported();
  const [chosen, setChosen] = useState<PreviewTransport>(storedTransport);
  const transport = supported ? chosen : "hls";
  const choose = (next: PreviewTransport) => {
    setChosen(next);
    try {
      localStorage.setItem(PREVIEW_TRANSPORT_KEY, next);
    } catch {
      // Storage blocked: the choice lasts this visit.
    }
  };
  return (
    <div className="space-y-2">
      <div role="radiogroup" aria-label="Preview transport" className="join">
        {(["hls", "moq"] as const).map((value) => (
          <button
            key={value}
            type="button"
            role="radio"
            aria-checked={transport === value}
            className={`btn btn-xs join-item ${transport === value ? "btn-active" : ""}`}
            disabled={value === "moq" && !supported}
            title={value === "moq" && !supported ? "This browser lacks WebTransport or WebCodecs" : undefined}
            onClick={() => choose(value)}
          >
            {value.toUpperCase()}
          </button>
        ))}
      </div>
      {transport === "moq" ? (
        <MoqPreview key="moq" pipelineId={props.pipelineId} label="program" />
      ) : (
        <Preview key="hls" url={programPreviewUrl(props.pipelineId)} label="program" />
      )}
    </div>
  );
}

function GraphView(props: {
  columns: readonly GraphColumn[] | null;
  selected: string | null;
  onSelect: (group: EgressGroup) => void;
}) {
  if (props.columns === null) return <p className="opacity-70">Loading graph…</p>;
  return (
    <ol className="flex items-stretch gap-2 overflow-x-auto">
      {props.columns.map((column, index) => (
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
              <li key={group.key} aria-label={`Destinations ${group.protocol ?? "unknown"}`}>
                <button
                  type="button"
                  aria-pressed={props.selected === group.key}
                  aria-label={`Inspect ${groupName(group)} group from ${group.upstream}`}
                  className={`min-w-40 rounded border-l-4 bg-base-100 p-2 text-left text-sm ${groupBorder(
                    group,
                  )} ${props.selected === group.key ? "ring-2 ring-primary" : ""}`}
                  onClick={() => props.onSelect(group)}
                >
                  <div className="text-xs uppercase opacity-60">egress group</div>
                  <div className="font-medium">{groupName(group)}</div>
                  <div className="text-xs opacity-70">
                    {group.delivering} / {group.outputs} delivering
                    {group.attention > 0 && ` · ${group.attention} degraded`}
                  </div>
                </button>
              </li>
            ))}
          </ul>
        </li>
      ))}
    </ol>
  );
}

function GroupTable(props: {
  groups: readonly EgressGroup[] | null;
  selected: string | null;
  onSelect: (group: EgressGroup) => void;
}) {
  if (props.groups === null) return <p className="opacity-70">Loading graph…</p>;
  if (props.groups.length === 0) return <p className="opacity-70">No destinations.</p>;
  return (
    <table className="table table-sm" aria-label="Destination groups">
      <thead>
        <tr>
          <th>Protocol</th>
          <th>Fed by</th>
          <th>Delivering</th>
          <th>Degraded</th>
          <th />
        </tr>
      </thead>
      <tbody>
        {props.groups.map((group) => (
          <tr key={group.key}>
            <td>{groupName(group)}</td>
            <td>{group.upstream}</td>
            <td className="tabular-nums">
              {group.delivering} / {group.outputs}
            </td>
            <td className="tabular-nums">{group.attention}</td>
            <td>
              <button
                type="button"
                className="btn btn-xs"
                aria-pressed={props.selected === group.key}
                onClick={() => props.onSelect(group)}
              >
                Inspect
              </button>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function Timeline(props: { pipelineId: string; outputs: readonly OutputState[] }) {
  const events = useLifecycleEvents(props.pipelineId);
  const names = useMemo(
    () => new Map(props.outputs.map((output) => [output.id, output.name])),
    [props.outputs],
  );
  if (events.value === null) return <p className="opacity-70">Loading events…</p>;
  const latest = [...events.value.events].sort((a, b) => b.seq - a.seq);
  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between text-xs opacity-70">
        <span>Newest first · the engine keeps only its latest events</span>
        <Freshness atMs={events.atMs} />
      </div>
      {latest.length === 0 ? (
        <p className="opacity-70">No kept lifecycle event for this pipeline.</p>
      ) : (
        <ol className="space-y-1 text-sm" aria-label="Pipeline timeline">
          {latest.map((event) => (
            <li key={event.seq} className="grid grid-cols-[7rem_1fr] gap-2">
              <time className="tabular-nums opacity-70" dateTime={event.timestamp}>
                {new Date(event.timestamp).toLocaleTimeString()}
              </time>
              <span>
                {event.kind}
                {event.outputId && ` · ${names.get(event.outputId) ?? event.outputId}`}
                {event.phase && ` · ${event.phase}`}
                {event.error && <span className="opacity-70"> · {event.error}</span>}
              </span>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

function ratio(value: number | null): string {
  return value === null ? "--" : value.toFixed(3);
}

function GroupInspector(props: {
  group: EgressGroup;
  outputs: readonly OutputState[];
  egresses: readonly TelemetryEgress[] | null;
  shards: readonly EgressShardStatus[] | null;
  shardsAtMs: number | null;
}) {
  const inspection = inspectGroup(props.group, props.outputs, props.egresses, props.shards);
  const title = `${groupName(props.group)} group · ${props.group.upstream}`;
  return (
    <section className="card bg-base-200" aria-label={`Selected ${groupName(props.group)} group`}>
      <div className="card-body gap-3 p-4">
        <div className="flex items-center gap-2">
          <h3 className="card-title text-base">Selected · {title}</h3>
          <span
            className={`badge badge-sm ${
              inspection.degraded.length > 0 ? "badge-warning" : "badge-success"
            }`}
          >
            {inspection.degraded.length > 0 ? "attention" : "healthy"}
          </span>
        </div>
        <div className="grid gap-4 md:grid-cols-2">
          <div>
            <h4 className="text-xs font-semibold uppercase opacity-70">Useful delivery</h4>
            <dl className="grid grid-cols-[1fr_auto] gap-x-3 text-sm" aria-label="Useful delivery">
              <dt>Destinations delivering</dt>
              <dd className="tabular-nums">
                {inspection.delivering} / {inspection.outputs}
              </dd>
              <dt>Min delivery ratio</dt>
              <dd className="tabular-nums">{ratio(inspection.minDeliveryRatio)}</dd>
              <dt>Worst feed lag</dt>
              <dd className="tabular-nums">{inspection.worstFeedLag ?? "--"}</dd>
              <dt>Resyncs</dt>
              <dd className="tabular-nums">{inspection.resyncs}</dd>
            </dl>
          </div>
          <div>
            <div className="flex items-center justify-between">
              <h4 className="text-xs font-semibold uppercase opacity-70">Execution owner</h4>
              <Freshness atMs={props.shardsAtMs} />
            </div>
            {inspection.shards.length === 0 ? (
              <p className="text-sm opacity-70">No running output of this group is on a fabric shard.</p>
            ) : (
              <ul className="space-y-1 text-sm" aria-label="Fabric shards">
                {inspection.shards.map((shard) => (
                  <li key={`${shard.feedId}#${shard.shardIndex}`}>
                    shard {shard.shardIndex}
                    {shard.status === null ? (
                      <span className="opacity-70"> · not in the latest sample</span>
                    ) : (
                      <>
                        {" · "}
                        {shard.status.state} · queue {shard.status.commandDepth} /{" "}
                        {shard.status.commandCapacity} · progress{" "}
                        {shard.status.progressAgeMs === null
                          ? "none yet"
                          : `${shard.status.progressAgeMs} ms ago`}
                      </>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
        <div>
          <h4 className="text-xs font-semibold uppercase opacity-70">Evidence</h4>
          {inspection.degraded.length === 0 ? (
            <p className="text-sm">No output of this group has a diagnosed fault.</p>
          ) : (
            <ul className="space-y-1 text-sm" aria-label="Diagnosed outputs">
              {inspection.degraded.map((output) => (
                <li key={output.id}>
                  <b>{output.name}</b>: {output.layer}: {output.cause}
                  {output.ruledOut.length > 0 && (
                    <span className="opacity-70"> · ruled out {output.ruledOut.join(", ")}</span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </section>
  );
}
