import { useMemo } from "react";
import type { ReactNode } from "react";

import type { AlertsSnapshot } from "../core/api-types.js";
import type { CapacitySnapshot } from "../core/generated/runtime-state.js";
import type { SystemMetrics } from "../types.js";
import { degradedTotal } from "./fleet-history.js";
import type { FleetSample } from "./fleet-history.js";
import { buildFleetModel, describeCause, worstFirst } from "./fleet-model.js";
import type { AttentionItem, FleetModel, FleetPipelineRow } from "./fleet-model.js";
import { formatBps, formatPercent } from "./format.js";
import { PipelineTable } from "./pipeline-table.js";
import { Freshness, useOpenAlerts } from "./polled.js";
import type { Polled } from "./polled.js";
import { PlaceLink } from "./router.js";
import { Sparkline } from "./sparkline.js";
import type { LiveSnapshot } from "./store.js";

export function Fleet({ live }: { live: LiveSnapshot }) {
  const model = useMemo(
    () => (live.state ? buildFleetModel(live.state, live.rates) : null),
    [live.state, live.rates],
  );
  const alerts = useOpenAlerts();
  if (model === null) {
    return <p className="opacity-70">Waiting for runtime state…</p>;
  }
  return (
    <div className="space-y-4">
      <Headline
        model={model}
        capacity={live.metrics?.capacity ?? null}
        alerts={alerts}
      />
      <div className="grid gap-4 lg:grid-cols-2">
        <Attention items={model.attention} />
        <Headroom metrics={live.metrics} />
      </div>
      <History history={live.history} />
      <FleetTable rows={model.pipelines} />
    </div>
  );
}

/** The fleet table shows at most this many pipelines; Pipelines lists all. */
const FLEET_TABLE_ROWS = 10;

function FleetTable({ rows }: { rows: readonly FleetPipelineRow[] }) {
  const shown = useMemo(() => worstFirst(rows).slice(0, FLEET_TABLE_ROWS), [rows]);
  return (
    <section className="card bg-base-200" aria-labelledby="fleet-table">
      <div className="card-body gap-3">
        <div className="flex items-center justify-between">
          <h3 id="fleet-table" className="card-title text-base">
            Fleet
            <span className="text-xs font-normal opacity-70">worst first</span>
          </h3>
          {rows.length > shown.length && (
            <PlaceLink className="link text-sm" place={{ workspace: "pipelines", pipelineId: null }}>
              All {rows.length} pipelines
            </PlaceLink>
          )}
        </div>
        {rows.length === 0 ? (
          <p className="opacity-70">No pipeline yet.</p>
        ) : (
          <PipelineTable rows={shown} label="Fleet pipelines" />
        )}
      </div>
    </section>
  );
}

function headline(model: FleetModel): string {
  const issues = model.attention.length;
  const pending = model.outputsPending;
  const starting = `${pending} output${pending === 1 ? "" : "s"} starting or not flowing yet`;
  if (issues > 0) {
    const causes = `${issues} ${issues === 1 ? "cause needs" : "causes need"} attention`;
    return pending > 0 ? `${causes} · ${starting}` : causes;
  }
  if (pending > 0) return starting;
  return model.outputsWanted === 0
    ? "No output is wanted running"
    : "Every wanted output is delivering";
}

function Headline(props: {
  model: FleetModel;
  capacity: CapacitySnapshot | null;
  alerts: Polled<AlertsSnapshot>;
}) {
  const { model, capacity } = props;
  const alerts = props.alerts.value;
  const critical = alerts?.alerts.filter((alert) => alert.severity === "critical").length ?? 0;
  return (
    <section aria-labelledby="fleet-headline">
      <h2 id="fleet-headline" className="text-xl font-semibold">
        {headline(model)}
      </h2>
      <div className="stats stats-vertical mt-3 w-full shadow sm:stats-horizontal">
        <Stat
          title="Pipelines healthy"
          value={`${model.pipelinesHealthy} / ${model.pipelinesTotal}`}
          detail={`${model.pipelinesReceiving} receiving media`}
        />
        <Stat
          title="Outputs delivering"
          value={`${model.outputsDelivering} / ${model.outputsWanted}`}
          detail="of the outputs wanted running"
        />
        <Stat
          title="Egress"
          value={formatBps(model.egressBps)}
          detail="bit rate across active outputs"
        />
        <Stat
          title="Shared work"
          value={capacity ? `${capacity.uniqueStages} stages` : "--"}
          detail={
            capacity
              ? `serve ${capacity.activeLeaves} active outputs`
              : "no capacity sample yet"
          }
        />
        <Stat
          title="Open incidents"
          value={alerts ? String(alerts.alerts.length) : "--"}
          detail={
            alerts ? (
              <span className="flex items-center gap-2">
                {critical} critical · {alerts.alerts.length - critical} warning
                <Freshness atMs={props.alerts.atMs} />
              </span>
            ) : (
              "loading alerts"
            )
          }
        />
      </div>
    </section>
  );
}

function Stat(props: { title: string; value: string; detail: ReactNode }) {
  return (
    <div className="stat">
      <div className="stat-title">{props.title}</div>
      <div className="stat-value text-2xl">{props.value}</div>
      <div className="stat-desc">{props.detail}</div>
    </div>
  );
}

/** The store's fleet-wide samples: how headroom, delivery and faults moved. */
function History({ history }: { history: readonly FleetSample[] }) {
  const last = history[history.length - 1];
  const rows: Array<[string, (sample: FleetSample) => number | null, string, number | undefined]> = [
    [
      "Headroom",
      (sample) => sample.headroomPercent,
      formatPercent(last?.headroomPercent ?? null),
      100,
    ],
    [
      "Outputs delivering",
      (sample) => sample.deliveringPercent,
      formatPercent(last?.deliveringPercent ?? null),
      100,
    ],
    [
      "Degraded outputs",
      degradedTotal,
      last ? String(degradedTotal(last) ?? "--") : "--",
      undefined,
    ],
  ];
  return (
    <section className="card bg-base-200" aria-labelledby="fleet-history">
      <div className="card-body gap-3">
        <h3 id="fleet-history" className="card-title text-base">
          Fleet history
          <span className="text-xs font-normal opacity-70">
            since this page opened, up to 15 min · {history.length} samples · headroom
            projected from the calibration priors, raised by the measured egress queue
            when shards run
          </span>
        </h3>
        {last === undefined ? (
          <p className="opacity-70">Collecting the first sample…</p>
        ) : (
          <dl className="space-y-2">
            {rows.map(([label, pick, now, top]) => (
              <div key={label} className="grid grid-cols-[12rem_1fr_7rem] items-center gap-3">
                <dt className="text-sm">{label}</dt>
                <dd>
                  <Sparkline history={history} pick={pick} label={`${label} over time`} top={top} />
                </dd>
                <dd className="text-right tabular-nums" aria-label={`${label} now`}>
                  {now}
                </dd>
              </div>
            ))}
          </dl>
        )}
        {last?.degraded && degradedTotal(last) ? (
          <p className="text-sm opacity-70">
            Now by layer:{" "}
            {Object.entries(last.degraded)
              .filter(([, count]) => count > 0)
              .map(([layer, count]) => `${layer} ${count}`)
              .join(" · ")}
          </p>
        ) : null}
      </div>
    </section>
  );
}

function Attention({ items }: { items: readonly AttentionItem[] }) {
  return (
    <section className="card bg-base-200" aria-labelledby="fleet-attention">
      <div className="card-body">
        <h3 id="fleet-attention" className="card-title text-base">
          Needs attention
          <span className="text-xs font-normal opacity-70">
            ranked by outputs affected
          </span>
        </h3>
        {items.length === 0 ? (
          <p className="opacity-70">No degraded output.</p>
        ) : (
          <ul className="space-y-2">
            {items.map((item) => (
              <li key={item.key} className="rounded bg-base-100 p-2">
                <div className="font-medium">
                  <PlaceLink
                    className="link"
                    place={{ workspace: "operate", pipelineId: item.pipelineId }}
                  >
                    {item.pipelineName}
                  </PlaceLink>{" "}
                  · {item.layer}: {describeCause(item.cause)}
                </div>
                <div className="text-sm opacity-70">
                  {item.outputs.length} output
                  {item.outputs.length === 1 ? "" : "s"}:{" "}
                  {item.outputs.join(", ")}
                  {item.ruledOut.length > 0 &&
                    ` · ruled out ${item.ruledOut.join(", ")}`}
                </div>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
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
  const projected = capacity ? serviceCenterRows(capacity) : [];
  return (
    <section className="card bg-base-200" aria-labelledby="fleet-headroom">
      <div className="card-body gap-3">
        <h3 id="fleet-headroom" className="card-title text-base">
          Headroom
        </h3>
        <Meters title="Measured host use" rows={measured} />
        <p className="text-sm opacity-70">
          Network: ↓ {formatBps((metrics?.network?.downloadKbps ?? NaN) * 1000)}{" "}
          · ↑ {formatBps((metrics?.network?.uploadKbps ?? NaN) * 1000)}
        </p>
        {capacity && (
          <>
            {capacity.flowMeasured && (
              <p className="text-sm">
                Egress flow (measured):{" "}
                <span
                  className={`badge badge-sm ${
                    capacity.flow.status === "healthy"
                      ? "badge-success"
                      : "badge-warning"
                  }`}
                >
                  {capacity.flow.status}
                </span>{" "}
                · command queue {formatPercent(capacity.flow.utilization * 100)}{" "}
                · progress age {Math.round(capacity.flow.delayMs)} ms
              </p>
            )}
            <Meters
              title={`Projected service-center load · calibration priors, observe-only · hottest: ${capacity.hottestCenter}`}
              rows={projected}
            />
          </>
        )}
      </div>
    </section>
  );
}

/** The projected service-center loads of one capacity snapshot, in percent. */
export function serviceCenterRows(
  capacity: CapacitySnapshot,
): Array<[string, number | null | undefined]> {
  return [
    ["Hottest shard", capacity.hottestShardUtil * 100],
    ["NIC", capacity.nicUtil * 100],
    ["FFmpeg", capacity.ffmpegUtil * 100],
    ["Memory", capacity.memoryUtil * 100],
    ["Disk", capacity.diskUtil * 100],
  ];
}

export function Meters(props: {
  title: string;
  rows: Array<[string, number | null | undefined]>;
}) {
  return (
    <div>
      <h4 className="mb-1 text-xs opacity-70">{props.title}</h4>
      <dl className="space-y-1">
        {props.rows.map(([label, value]) => (
          <div
            key={label}
            className="grid grid-cols-[7rem_1fr_3rem] items-center gap-2"
          >
            <dt className="text-sm">{label}</dt>
            <dd>
              <progress
                className="progress w-full"
                value={value ?? 0}
                max={100}
                aria-label={`${props.title}: ${label}`}
              />
            </dd>
            <dd className="text-right text-sm tabular-nums">
              {formatPercent(value)}
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}
