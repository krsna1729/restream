import { useEffect, useMemo, useState } from "react";

import { listMediaFiles } from "../core/api.js";
import type { MediaFile } from "../core/api-types.js";
import { ChipGroup, SearchField, ShareBar } from "../ui/controls.js";
import { Kpi } from "../ui/kpi.js";
import { PageHeader } from "../ui/page-header.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { buildFleetModel, filterPipelineRows, OPERATIONAL, pipelineHealth } from "./fleet-model.js";
import type { HealthFilter, OperationalFilter } from "./fleet-model.js";
import { PipelineForm } from "./pipeline-form.js";
import { PipelineTable } from "./pipeline-table.js";
import { protocolMix, sharedStages } from "./pipelines-model.js";
import type { SharedStages } from "./pipelines-model.js";
import { PROTOCOL_COLOR, PROTOCOL_LABEL } from "./protocol-style.js";
import { useGo } from "./router.js";
import type { LiveSnapshot } from "./store.js";

const HEALTH_FILTERS: ReadonlyArray<[HealthFilter, string]> = [
  ["all", "All"],
  ["warning", "Degraded"],
  ["info", "Starting"],
  ["ok", "Healthy"],
  ["idle", "Idle"],
];

const OPERATIONAL_CARDS: ReadonlyArray<[OperationalFilter, string, string]> = [
  ["attention", "Needs attention", "a wanted destination is degraded"],
  ["standby", "Has warm standby", "a backup input is connected"],
  ["bonded", "Uses bonded SRT", "ingest over an SRT group"],
  ["recording", "Recording active", "a file is being written"],
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
  const rows = useMemo(() => (live.state ? buildFleetModel(live.state, live.rates).pipelines : null), [live.state, live.rates]);
  const sharing = useMemo(() => live.state && sharedStages(live.state.pipelines, live.state.outputs), [live.state]);
  const healthCounts = useMemo(() => {
    const counts: Record<HealthFilter, number> = { all: rows?.length ?? 0, warning: 0, info: 0, ok: 0, idle: 0 };
    for (const row of rows ?? []) counts[pipelineHealth(row).tone] += 1;
    return counts;
  }, [rows]);
  const go = useGo();
  if (rows === null || sharing === null) {
    return <EmptyState title="Waiting for runtime state…" />;
  }
  const shown = filterPipelineRows(rows, query, health, operational);
  const mix = protocolMix(rows);
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Inventory"
        title={`${rows.length.toLocaleString()} pipelines`}
        subtitle="Attention first, then the pipelines carrying the most destinations."
        actions={
          <button type="button" className="btn btn-primary btn-sm" aria-expanded={creating} onClick={() => setCreating(!creating)}>
            {creating ? "Close" : "New pipeline"}
          </button>
        }
      />
      {creating && (
        <Panel>
          <PipelineForm
            stored={null}
            files={files}
            keyLocked={false}
            onSaved={(pipeline) => go({ workspace: "operate", pipelineId: pipeline.id })}
          />
        </Panel>
      )}
      <div className="grid gap-4 lg:grid-cols-3">
        <Panel id="protocol-mix" title="Protocol mix" subtitle="every destination, by protocol">
          <ShareBar
            label="Destinations by protocol"
            items={mix.map((share) => ({
              key: share.protocol,
              label: PROTOCOL_LABEL[share.protocol],
              value: share.outputs,
              color: PROTOCOL_COLOR[share.protocol],
            }))}
          />
        </Panel>
        <StageSharing sharing={sharing} />
        <Panel id="operational-filters" title="Operational filters">
          <div className="grid grid-cols-2 gap-2">
            {OPERATIONAL_CARDS.map(([value, label, hint]) => {
              const count = rows.filter(OPERATIONAL[value]).length;
              const active = operational === value;
              return (
                <button
                  key={value}
                  type="button"
                  aria-pressed={active}
                  title={hint}
                  onClick={() => setOperational(active ? null : value)}
                  className={`flex min-h-14 flex-col items-start justify-center rounded-field border px-3 py-2 text-left transition-colors ${active ? "border-primary bg-primary/15" : "border-base-content/15 hover:border-base-content/35"}`}
                >
                  <span className="text-sm font-medium">{label}</span>
                  <span className={`tabular text-xs ${value === "attention" && count > 0 ? "text-warning" : "text-base-content/70"}`}>
                    {count.toLocaleString()} pipeline{count === 1 ? "" : "s"}
                  </span>
                </button>
              );
            })}
          </div>
        </Panel>
      </div>
      <Panel
        flush
        id="pipelines-list"
        title="Pipelines"
        subtitle={`${shown.length.toLocaleString()} of ${rows.length.toLocaleString()}`}
        actions={<SearchField value={query} onChange={setQuery} placeholder="Name or id" label="Search pipelines" className="w-full sm:w-56" />}
      >
        <div className="border-b border-base-content/10 px-4 py-2">
          <ChipGroup<HealthFilter>
            label="Health"
            value={health}
            onChange={setHealth}
            chips={HEALTH_FILTERS.map(([value, label]) => ({ value, label, count: healthCounts[value], alert: value === "warning" }))}
          />
        </div>
        <PipelineTable rows={shown} label="Pipelines" maxHeight={640} trends={live.ingestTrend} />
      </Panel>
    </div>
  );
}

function StageSharing({ sharing }: { sharing: SharedStages }) {
  const largest = sharing.groups[0];
  const ratio = sharing.stagedOutputs === 0 ? null : sharing.sharedOutputs / sharing.stagedOutputs;
  return (
    <Panel id="stage-sharing" title="Shared-stage efficiency" subtitle="one stage's work serves every output that reads it">
      {largest ? (
        <div className="grid grid-cols-2 gap-3">
          <Kpi
            label="Outputs sharing"
            value={ratio === null ? "--" : `${Math.round(ratio * 100)}%`}
            meter={ratio}
            meterTone="ok"
            foot={`${sharing.sharedOutputs.toLocaleString()} of ${sharing.stagedOutputs.toLocaleString()} running outputs`}
          />
          <Kpi label="Largest group" value={`${largest.outputs} outputs`} foot={`one ${largest.kind} stage · ${largest.pipelineName}`} />
        </div>
      ) : (
        <p className="text-sm text-base-content/70">
          {sharing.stagedOutputs === 0 ? "No running output reads a stage." : `${sharing.stagedOutputs} running outputs, each on its own stage.`}
        </p>
      )}
    </Panel>
  );
}
