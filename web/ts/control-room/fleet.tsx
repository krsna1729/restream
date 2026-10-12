import { useMemo } from "react";

import type { SystemMetrics } from "../types.js";
import {
  buildFleetModel,
  describeCause,
  pipelineHealth,
} from "./fleet-model.js";
import type {
  AttentionItem,
  FleetModel,
  FleetPipelineRow,
  PipelineHealth,
} from "./fleet-model.js";
import { formatBps, formatPercent } from "./format.js";
import type { LiveSnapshot } from "./store.js";

export function Fleet({ live }: { live: LiveSnapshot }) {
  const model = useMemo(
    () => (live.state ? buildFleetModel(live.state, live.rates) : null),
    [live.state, live.rates],
  );
  if (model === null) {
    return <p className="opacity-70">Waiting for runtime state…</p>;
  }
  return (
    <div className="space-y-4">
      <Headline model={model} />
      <div className="grid gap-4 lg:grid-cols-2">
        <Attention items={model.attention} />
        <Headroom metrics={live.metrics} />
      </div>
      <PipelineTable model={model} />
    </div>
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

function Headline({ model }: { model: FleetModel }) {
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
      </div>
    </section>
  );
}

function Stat(props: { title: string; value: string; detail: string }) {
  return (
    <div className="stat">
      <div className="stat-title">{props.title}</div>
      <div className="stat-value text-2xl">{props.value}</div>
      <div className="stat-desc">{props.detail}</div>
    </div>
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
                  {item.pipelineName} · {item.layer}:{" "}
                  {describeCause(item.cause)}
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
  const projected: Array<[string, number | null | undefined]> = capacity
    ? [
        ["Hottest shard", capacity.hottestShardUtil * 100],
        ["NIC", capacity.nicUtil * 100],
        ["FFmpeg", capacity.ffmpegUtil * 100],
        ["Memory", capacity.memoryUtil * 100],
        ["Disk", capacity.diskUtil * 100],
      ]
    : [];
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

function Meters(props: {
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

function PipelineTable({ model }: { model: FleetModel }) {
  return (
    <section aria-labelledby="fleet-pipelines">
      <h3 id="fleet-pipelines" className="mb-2 font-semibold">
        Pipelines{" "}
        <span className="text-xs font-normal opacity-70">
          attention first, then size
        </span>
      </h3>
      <div className="overflow-x-auto">
        <table className="table table-sm">
          <thead>
            <tr>
              <th>Name</th>
              <th>Source</th>
              <th>Outputs</th>
              <th>Flow</th>
              <th>Health</th>
            </tr>
          </thead>
          <tbody>
            {model.pipelines.map((row) => (
              <tr key={row.id}>
                <td className="font-medium">{row.name}</td>
                <td>
                  {row.source
                    ? `${row.source.protocol.toUpperCase()}${
                        row.source.videoCodec ? ` · ${row.source.videoCodec}` : ""
                      }`
                    : "no input"}
                </td>
                <td className="tabular-nums">
                  {row.delivering} / {row.wanted}
                </td>
                <td className="tabular-nums">
                  {formatBps(row.ingestBps)} → {formatBps(row.egressBps)}
                </td>
                <td>
                  <HealthBadge row={row} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

const HEALTH_BADGE: Record<PipelineHealth["tone"], string> = {
  warning: "badge-warning",
  info: "badge-info",
  idle: "badge-ghost",
  ok: "badge-success",
};

function HealthBadge({ row }: { row: FleetPipelineRow }) {
  const health = pipelineHealth(row);
  return (
    <span className={`badge badge-sm ${HEALTH_BADGE[health.tone]}`}>
      {health.label}
    </span>
  );
}
