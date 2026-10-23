import { useMemo, useState } from "react";

import { programPreviewUrl } from "../core/api.js";
import type { TelemetryEgress } from "../core/api-types.js";
import type { OutputState } from "../core/generated/runtime-state.js";
import type { EgressShardStatus } from "../types.js";
import { inspectGroup } from "./operate-group-model.js";
import type { EgressGroup, GraphColumn } from "./operate-model.js";
import { KeyValue } from "../ui/kpi.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
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
  if (group.attention > 0) return "border-l-warning";
  return group.delivering === group.outputs ? "border-l-success" : "border-l-base-content/20";
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
    <div className="flex min-w-0 flex-col gap-4">
      <Panel
        id="operate-graph"
        title="Media path"
        subtitle="shared work drawn once"
        actions={<Freshness atMs={props.atMs} />}
      >
        <div role="tablist" aria-label="Media path views" className="scrollbar-thin mb-3 flex gap-1 overflow-x-auto">
          {TABS.map(([value, label]) => (
            <button
              key={value}
              type="button"
              role="tab"
              aria-selected={tab === value}
              className={`touch-36 h-8 shrink-0 rounded-field px-3 text-sm ${tab === value ? "bg-primary/15 font-semibold" : "text-base-content/70 hover:bg-base-content/5"}`}
              onClick={() => setTab(value)}
            >
              {label}
            </button>
          ))}
        </div>
        <div role="tabpanel" aria-label={TABS.find(([value]) => value === tab)?.[1]}>
          {tab === "graph" && (
            <GraphView columns={props.columns} selected={selectedKey} onSelect={select} onShowAll={() => setTab("destinations")} />
          )}
          {tab === "destinations" && (
            <GroupTable groups={props.columns === null ? null : groups} selected={selectedKey} onSelect={select} />
          )}
          {tab === "timeline" && <Timeline pipelineId={props.pipelineId} outputs={props.outputs} />}
          {tab === "preview" && <ProgramPreview pipelineId={props.pipelineId} />}
        </div>
      </Panel>
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

/** Egress groups the graph draws before folding the rest into a link. */
const GRAPH_GROUP_LIMIT = 8;

function GraphView(props: {
  columns: readonly GraphColumn[] | null;
  selected: string | null;
  onSelect: (group: EgressGroup) => void;
  onShowAll: () => void;
}) {
  if (props.columns === null) return <p className="text-base-content/70">Loading graph…</p>;
  return (
    <ol className="scrollbar-thin flex flex-col items-stretch gap-2 lg:flex-row lg:items-center lg:overflow-x-auto lg:pb-2">
      {props.columns.map((column, index) => {
        const groups = [...column.groups].sort((a, b) => b.attention - a.attention || b.outputs - a.outputs);
        const shownGroups = groups.slice(0, GRAPH_GROUP_LIMIT);
        const hidden = groups.slice(GRAPH_GROUP_LIMIT);
        return (
          <li key={index} className="flex flex-col items-stretch gap-2 lg:flex-row lg:items-center">
            {index > 0 && (
              <span aria-hidden="true" className="self-center text-base-content/40 lg:rotate-0 max-lg:rotate-90">
                →
              </span>
            )}
            <ul className="grid grid-cols-1 gap-2 sm:grid-cols-2 lg:flex lg:flex-col">
              {column.stages.map((stage) => (
                <li
                  key={stage.id}
                  aria-label={`Stage ${stage.label}`}
                  className={`rounded-field border border-l-4 border-base-content/10 bg-base-100 px-3 py-2 text-sm lg:w-44 ${stage.active ? "border-l-success" : "border-l-base-content/20"}`}
                >
                  <div className="kicker">{stage.type.replace("_", " ")}</div>
                  <div className="truncate font-medium">{stage.label}</div>
                  <div className="tabular text-xs text-base-content/70">
                    {stage.consumers} consumer{stage.consumers === 1 ? "" : "s"}
                    {stage.packetsPerSec !== null && ` · ${stage.packetsPerSec.toFixed(0)} pkt/s`}
                  </div>
                </li>
              ))}
              {shownGroups.map((group) => (
                <li key={group.key} aria-label={`Destinations ${group.protocol ?? "unknown"}`}>
                  <button
                    type="button"
                    aria-pressed={props.selected === group.key}
                    aria-label={`Inspect ${groupName(group)} group from ${group.upstream}`}
                    className={`w-full rounded-field border border-l-4 border-base-content/10 bg-base-100 px-3 py-2 text-left text-sm transition-colors hover:border-base-content/30 lg:w-44 ${groupBorder(group)} ${props.selected === group.key ? "ring-2 ring-primary" : ""}`}
                    onClick={() => props.onSelect(group)}
                  >
                    <div className="kicker">egress · {group.upstream}</div>
                    <div className="font-medium">{groupName(group)}</div>
                    <div className="tabular text-xs text-base-content/70">
                      {group.delivering} / {group.outputs} delivering
                      {group.attention > 0 && <span className="text-warning"> · {group.attention} degraded</span>}
                    </div>
                  </button>
                </li>
              ))}
              {hidden.length > 0 && (
                <li>
                  <button type="button" className="btn btn-ghost btn-sm w-full lg:w-44" onClick={props.onShowAll}>
                    +{hidden.length} more groups ({hidden.reduce((sum, group) => sum + group.outputs, 0)} destinations)
                  </button>
                </li>
              )}
            </ul>
          </li>
        );
      })}
    </ol>
  );
}

function GroupTable(props: {
  groups: readonly EgressGroup[] | null;
  selected: string | null;
  onSelect: (group: EgressGroup) => void;
}) {
  if (props.groups === null) return <p className="text-base-content/70">Loading graph…</p>;
  if (props.groups.length === 0) return <p className="text-base-content/70">No destinations.</p>;
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
  if (events.value === null) return <p className="text-base-content/70">Loading events…</p>;
  const latest = [...events.value.events].sort((a, b) => b.seq - a.seq);
  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between text-xs text-base-content/70">
        <span>Newest first · the engine keeps only its latest events</span>
        <Freshness atMs={events.atMs} />
      </div>
      {latest.length === 0 ? (
        <p className="text-base-content/70">No kept lifecycle event for this pipeline.</p>
      ) : (
        <ol className="space-y-1 text-sm" aria-label="Pipeline timeline">
          {latest.map((event) => (
            <li key={event.seq} className="grid grid-cols-[7rem_1fr] gap-2">
              <time className="tabular-nums text-base-content/70" dateTime={event.timestamp}>
                {new Date(event.timestamp).toLocaleTimeString()}
              </time>
              <span>
                {event.kind}
                {event.outputId && ` · ${names.get(event.outputId) ?? event.outputId}`}
                {event.phase && ` · ${event.phase}`}
                {event.error && <span className="text-base-content/70"> · {event.error}</span>}
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
    <Panel
      title={`Selected · ${title}`}
      actions={
        <StatusBadge tone={inspection.degraded.length > 0 ? "warn" : "ok"} dot>
          {inspection.degraded.length > 0 ? "attention" : "healthy"}
        </StatusBadge>
      }
    >
      <section aria-label={`Selected ${groupName(props.group)} group`} className="flex flex-col gap-4">
        <div className="grid gap-6 md:grid-cols-2">
          <div>
            <h4 className="kicker mb-2">Useful delivery</h4>
            <KeyValue
              rows={[
                ["Destinations delivering", `${inspection.delivering} / ${inspection.outputs}`],
                ["Min delivery ratio", ratio(inspection.minDeliveryRatio)],
                ["Worst feed lag", inspection.worstFeedLag ?? "--"],
                ["Resyncs", inspection.resyncs],
              ]}
            />
          </div>
          <div>
            <div className="mb-2 flex items-center justify-between">
              <h4 className="kicker">Execution owner</h4>
              <Freshness atMs={props.shardsAtMs} />
            </div>
            {inspection.shards.length === 0 ? (
              <p className="text-sm text-base-content/70">No running output of this group is on a fabric shard.</p>
            ) : (
              <ul className="flex flex-col gap-1 text-sm tabular" aria-label="Fabric shards">
                {inspection.shards.map((shard) => (
                  <li key={`${shard.feedId}#${shard.shardIndex}`}>
                    shard {shard.shardIndex}
                    {shard.status === null ? (
                      <span className="text-base-content/70"> · not in the latest sample</span>
                    ) : (
                      <>
                        {" · "}
                        <span className={shard.status.state === "healthy" ? "" : "text-error"}>{shard.status.state}</span> · queue{" "}
                        {shard.status.commandDepth} / {shard.status.commandCapacity} · progress{" "}
                        {shard.status.progressAgeMs === null ? "none yet" : `${shard.status.progressAgeMs} ms ago`}
                      </>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
        <div>
          <h4 className="kicker mb-2">Evidence</h4>
          {inspection.degraded.length === 0 ? (
            <p className="text-sm">No output of this group has a diagnosed fault.</p>
          ) : (
            <ul className="flex max-h-64 flex-col gap-1 overflow-y-auto text-sm" aria-label="Diagnosed outputs">
              {inspection.degraded.map((output) => (
                <li key={output.id} className="rounded-field border-l-2 border-warning bg-base-100 px-3 py-1.5">
                  <b>{output.name}</b>: {output.layer}: {output.cause}
                  {output.ruledOut.length > 0 && <span className="text-base-content/70"> · ruled out {output.ruledOut.join(", ")}</span>}
                </li>
              ))}
            </ul>
          )}
        </div>
      </section>
    </Panel>
  );
}
