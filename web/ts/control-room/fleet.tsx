import { useMemo, useState } from "react";

import type { AlertsSnapshot } from "../core/api-types.js";
import type { CapacitySnapshot } from "../core/generated/runtime-state.js";
import { Kpi, KpiRow, MeterRow } from "../ui/kpi.js";
import { PageHeader } from "../ui/page-header.js";
import { EmptyState, Panel } from "../ui/panel.js";
import { RawJson } from "../ui/raw-json.js";
import { TimeSeries } from "../ui/time-series.js";
import { StatusBadge, utilizationTone } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";
import type { SystemMetrics } from "../types.js";
import { degradedTotal } from "./fleet-history.js";
import type { FleetSample } from "./fleet-history.js";
import { attentionGroups, buildFleetModel, describeCause, roomFor, worstFirst } from "./fleet-model.js";
import type { AttentionGroup, FleetModel, FleetPipelineRow } from "./fleet-model.js";
import { counted, formatBps, formatPercent } from "./format.js";
import { PipelineTable } from "./pipeline-table.js";
import { Freshness, useOpenAlerts } from "./polled.js";
import type { Polled } from "./polled.js";
import { PlaceLink } from "./router.js";
import type { LiveSnapshot } from "./store.js";
import { LogsLink } from "./logs-link.js";
import { explainCause, troubleTitle } from "./trouble-model.js";

export function Fleet({ live }: { live: LiveSnapshot }) {
  const model = useMemo(
    () => (live.state ? buildFleetModel(live.state, live.rates) : null),
    [live.state, live.rates],
  );
  const alerts = useOpenAlerts();
  if (model === null) {
    return <EmptyState title="Waiting for runtime state…" detail="The dashboard is connecting to the engine's state stream." />;
  }
  const capacity = live.metrics?.capacity ?? null;
  const verdict = headline(model);
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow={`Fleet · ${model.pipelinesTotal.toLocaleString()} pipelines · ${(live.state?.outputs.length ?? 0).toLocaleString()} destinations`}
        title={verdict.text}
        tone={verdict.tone}
        subtitle={`${model.outputsDelivering.toLocaleString()} of ${model.outputsWanted.toLocaleString()} wanted destinations delivering · ${formatBps(model.egressBps)} out`}
      />
      <Headline model={model} capacity={capacity} alerts={alerts} ingestBps={live.metrics?.network?.downloadKbps} />
      <div className="grid gap-4 xl:grid-cols-[minmax(0,1.6fr)_minmax(0,1fr)]">
        <Attention items={model.attention} />
        <Headroom metrics={live.metrics} />
      </div>
      <History history={live.history} />
      <FleetTable rows={model.pipelines} />
      <RawJson value={{ capacity, metrics: live.metrics }} label="capacity + host metrics" />
    </div>
  );
}

function headline(model: FleetModel): { text: string; tone: Tone } {
  const issues = model.attention.length;
  const degraded = model.attention.reduce((sum, item) => sum + item.outputs.length, 0);
  const pending = model.outputsPending;
  if (issues > 0) {
    return {
      text: `${degraded.toLocaleString()} destination${degraded === 1 ? "" : "s"} need attention`,
      tone: "warn",
    };
  }
  if (pending > 0) return { text: `${pending} destination${pending === 1 ? "" : "s"} starting`, tone: "info" };
  return model.outputsWanted === 0
    ? { text: "No destination is wanted running", tone: "neutral" }
    : { text: "Every wanted destination is delivering", tone: "ok" };
}

function Headline(props: {
  model: FleetModel;
  capacity: CapacitySnapshot | null;
  alerts: Polled<AlertsSnapshot>;
  ingestBps: number | null | undefined;
}) {
  const { model, capacity } = props;
  const alerts = props.alerts.value;
  const critical = alerts?.alerts.filter((alert) => alert.severity === "critical").length ?? 0;
  const delivering = model.outputsWanted === 0 ? null : model.outputsDelivering / model.outputsWanted;
  const headroom = capacity ? Math.max(0, 1 - capacity.projectedUtilization) : null;
  return (
    <KpiRow max={6}>
      <Kpi
        label="Delivering"
        value={delivering === null ? "--" : formatPercent(delivering * 100)}
        tone={delivering === null ? undefined : delivering >= 0.98 ? "ok" : delivering >= 0.9 ? "warn" : "bad"}
        meter={delivering}
        meterTone={delivering === null ? "neutral" : delivering >= 0.98 ? "ok" : delivering >= 0.9 ? "warn" : "bad"}
        foot={`${model.outputsDelivering.toLocaleString()} / ${model.outputsWanted.toLocaleString()} destinations`}
      />
      <Kpi
        label="Pipelines live"
        value={`${model.pipelinesReceiving} / ${model.pipelinesTotal}`}
        foot={`${model.pipelinesHealthy} fully healthy`}
      />
      <Kpi label="Egress" value={formatBps(model.egressBps)} foot={`ingest ${formatBps((props.ingestBps ?? NaN) * 1000)}`} />
      <Kpi
        label="Open incidents"
        value={alerts ? alerts.alerts.length.toLocaleString() : "--"}
        tone={critical > 0 ? "bad" : alerts && alerts.alerts.length > 0 ? "warn" : undefined}
        foot={
          alerts ? (
            <span className="inline-flex items-center gap-2">
              {critical} critical · {alerts.alerts.length - critical} warning <Freshness atMs={props.alerts.atMs} />
            </span>
          ) : (
            "loading"
          )
        }
      />
      <Kpi
        label="Headroom"
        value={headroom === null ? "--" : formatPercent(headroom * 100)}
        meter={capacity?.projectedUtilization ?? null}
        tone={headroom === null ? undefined : utilizationTone(capacity?.projectedUtilization) === "ok" ? undefined : utilizationTone(capacity?.projectedUtilization)}
        foot={capacity ? roomWords(capacity) : "no capacity sample"}
      />
      <Kpi
        label="Shared work"
        value={capacity ? `${capacity.uniqueStages.toLocaleString()} stages` : "--"}
        foot={capacity ? `serve ${counted(capacity.activeLeaves, "active output")}` : "no capacity sample"}
      />
    </KpiRow>
  );
}

const LAYER_TONE: Record<AttentionGroup["layer"], Tone> = { ingest: "warn", feed: "bad", shard: "bad", destination: "warn" };

/** Shared causes first: one row per kind of trouble, its pipelines inside,
 * each with how many destinations it takes down. Never a wall of names. */
function Attention({ items }: { items: FleetModel["attention"] }) {
  const groups = useMemo(() => attentionGroups(items), [items]);
  const [open, setOpen] = useState<string | null>(null);
  const expanded = open ?? groups[0]?.key ?? null;
  return (
    <Panel
      id="fleet-attention"
      title="Needs attention"
      subtitle={groups.length ? `${groups.length} kinds of trouble · ranked by destinations affected` : undefined}
      flush
    >
      {groups.length === 0 ? (
        <EmptyState title="Nothing degraded" detail="Every wanted destination either delivers or is starting." tone="ok" />
      ) : (
        <ul className="divide-y divide-base-content/10">
          {groups.map((group) => {
            const isOpen = group.key === expanded;
            const shown = group.items.slice(0, 8);
            return (
              <li key={group.key}>
                <button
                  type="button"
                  aria-expanded={isOpen}
                  onClick={() => setOpen(isOpen ? "" : group.key)}
                  className="flex min-h-12 w-full items-center gap-3 px-4 py-2.5 text-left hover:bg-base-content/5"
                >
                  <StatusBadge tone={LAYER_TONE[group.layer]}>{group.layer}</StatusBadge>
                  <span className="flex min-w-0 flex-1 flex-col sm:flex-row sm:items-center sm:gap-3">
                    <span className="truncate font-medium sm:flex-1">{group.title}</span>
                    <span className="shrink-0 text-xs text-base-content/70 tabular sm:text-sm">
                      {counted(group.outputs, "destination")} · {counted(group.pipelines, "pipeline")}
                    </span>
                  </span>
                  <span aria-hidden="true" className={`text-base-content/50 transition-transform ${isOpen ? "rotate-90" : ""}`}>›</span>
                </button>
                {isOpen && (
                  <ul className="flex flex-col gap-1 px-4 pb-3">
                    {shown.map((item) => (
                      <li key={item.key} className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 rounded-field bg-base-100 px-3 py-2 text-sm">
                        <div className="min-w-0">
                          <span className="flex items-baseline gap-2">
                            <PlaceLink className="touch-36 inline-flex items-center font-medium hover:underline" place={{ workspace: "operate", pipelineId: item.pipelineId }}>
                              {item.pipelineName}
                            </PlaceLink>
                            <LogsLink pipelineId={item.pipelineId} />
                          </span>
                          <div
                            className="truncate text-xs text-base-content/70"
                            title={`${troubleTitle(explainCause(item.cause))}${item.ruledOut.length > 0 ? `\nRuled out: ${item.ruledOut.join(", ")}` : ""}`}
                          >
                            {describeCause(item.cause)}
                          </div>
                          <div className="truncate text-xs text-base-content/60">
                            {item.outputs.slice(0, 3).join(", ")}
                            {item.outputs.length > 3 && ` +${item.outputs.length - 3} more`}
                          </div>
                        </div>
                        <span className="tabular text-sm font-semibold">{item.outputs.length}</span>
                      </li>
                    ))}
                    {group.items.length > shown.length && (
                      <li className="px-3 pt-1 text-xs text-base-content/70">
                        +{group.items.length - shown.length} more pipelines ·{" "}
                        <PlaceLink className="link touch-36 inline-flex items-center" place={{ workspace: "pipelines", pipelineId: null }}>
                          see Pipelines
                        </PlaceLink>
                      </li>
                    )}
                  </ul>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </Panel>
  );
}

function Headroom({ metrics }: { metrics: SystemMetrics | null }) {
  const capacity = metrics?.capacity ?? null;
  const measured: Array<[string, number | null | undefined]> = [
    ["Host CPU", metrics?.cpu?.usagePercent],
    ["Engine CPU", metrics?.engine?.restreamCpuPercent],
    ["FFmpeg CPU", metrics?.engine?.externalFfmpegCpuPercent],
    ["Memory", metrics?.memory?.usedPercent],
    ["Disk", metrics?.disk?.usedPercent],
  ];
  return (
    <Panel
      id="fleet-headroom"
      title="Headroom"
      subtitle={capacity ? `hottest: ${capacity.hottestCenter}` : undefined}
      actions={
        capacity?.flowMeasured ? (
          <StatusBadge tone={capacity.flow.status === "healthy" ? "ok" : capacity.flow.status === "saturated" ? "warn" : "bad"} dot>
            egress {capacity.flow.status}
          </StatusBadge>
        ) : undefined
      }
    >
      <div className="flex flex-col gap-4">
        <Meters title="Measured host use" rows={measured} />
        <p className="text-sm text-base-content/70 tabular">
          Network ↓ {formatBps((metrics?.network?.downloadKbps ?? NaN) * 1000)} · ↑{" "}
          {formatBps((metrics?.network?.uploadKbps ?? NaN) * 1000)}
          {capacity?.flowMeasured &&
            ` · shard queue ${formatPercent(capacity.flow.utilization * 100)} · progress ${Math.round(capacity.flow.delayMs)} ms`}
        </p>
        {capacity && <Meters title="Projected load · calibration priors, observe-only" rows={serviceCenterRows(capacity)} />}
      </div>
    </Panel>
  );
}

/** The projected service-center loads of one capacity snapshot, in percent. */
export function serviceCenterRows(capacity: CapacitySnapshot): Array<[string, number | null | undefined]> {
  return [
    ["Hottest shard", capacity.hottestShardUtil * 100],
    ["NIC", capacity.nicUtil * 100],
    ["FFmpeg", capacity.ffmpegUtil * 100],
    ["Memory", capacity.memoryUtil * 100],
    ["Disk", capacity.diskUtil * 100],
  ];
}

/** Percent meters, amber from 80 (the safe line), red from 100. */
export function Meters(props: { title: string; rows: Array<[string, number | null | undefined]> }) {
  return (
    <div>
      <h4 className="kicker mb-2">{props.title}</h4>
      <div className="flex flex-col gap-1.5">
        {props.rows.map(([label, value]) => (
          <MeterRow
            key={label}
            label={label}
            value={value === null || value === undefined ? null : value / 100}
            display={formatPercent(value)}
          />
        ))}
      </div>
    </div>
  );
}

/** The store's fleet-wide samples: how delivery, headroom and faults moved. */
function History({ history }: { history: readonly FleetSample[] }) {
  if (history.length < 2) {
    return (
      <Panel id="fleet-history" title="Fleet history">
        <p className="text-sm text-base-content/70">Collecting samples…</p>
      </Panel>
    );
  }
  const last = history[history.length - 1];
  return (
    <Panel id="fleet-history" title="Fleet history" subtitle={`since this page opened, up to 15 min · ${history.length} samples`}>
      <div className="grid gap-6 lg:grid-cols-2">
        <TimeSeries
          samples={history}
          label="Delivery and headroom over time, percent"
          top={100}
          series={[
            { id: "delivering", label: "Delivering %", pick: (sample) => sample.deliveringPercent, color: "text-success", area: true, format: (value) => `${value.toFixed(1)}%` },
            { id: "headroom", label: "Headroom %", pick: (sample) => sample.headroomPercent, color: "text-info", format: (value) => `${value.toFixed(0)}%` },
          ]}
        />
        <TimeSeries
          samples={history}
          label="Degraded destinations by layer over time"
          aside={
            last.degraded
              ? `now: ${
                  Object.entries(last.degraded)
                    .filter(([, count]) => count > 0)
                    .map(([layer, count]) => `${layer} ${count}`)
                    .join(" · ") || "none"
                }`
              : undefined
          }
          series={[
            { id: "destination", label: "Destination", pick: (sample) => sample.degraded?.destination ?? null, color: "text-warning" },
            { id: "ingest", label: "Ingest", pick: (sample) => sample.degraded?.ingest ?? null, color: "text-secondary" },
            { id: "shard", label: "Shard", pick: (sample) => sample.degraded?.shard ?? null, color: "text-error" },
            { id: "feed", label: "Feed", pick: (sample) => sample.degraded?.feed ?? null, color: "text-info", dashed: true },
          ]}
        />
      </div>
    </Panel>
  );
}

/** The fleet table shows the worst pipelines; Pipelines lists all. */
const FLEET_TABLE_ROWS = 12;

function FleetTable({ rows }: { rows: readonly FleetPipelineRow[] }) {
  const shown = useMemo(() => worstFirst(rows).slice(0, FLEET_TABLE_ROWS), [rows]);
  return (
    <Panel
      id="fleet-table"
      title="Worst pipelines"
      subtitle="degraded first, then the most destinations"
      flush
      actions={
        rows.length > shown.length ? (
          <PlaceLink className="link touch-36 inline-flex items-center text-sm" place={{ workspace: "pipelines", pipelineId: null }}>
            All {rows.length.toLocaleString()} pipelines
          </PlaceLink>
        ) : undefined
      }
    >
      {rows.length === 0 ? <EmptyState title="No pipeline yet" /> : <PipelineTable rows={shown} label="Worst pipelines" />}
    </Panel>
  );
}

/** "room for about 120 more destinations · hottest: egress". */
function roomWords(capacity: CapacitySnapshot): string {
  const room = roomFor(capacity.projectedUtilization, capacity.activeLeaves);
  const hottest = `hottest: ${capacity.hottestCenter}`;
  if (room === null) return `${hottest}${capacity.flowMeasured ? " · measured" : " · projected"}`;
  return room === 0 ? `at the safe limit · ${hottest}` : `room for about ${counted(room, "more destination", "more destinations")} · ${hottest}`;
}
