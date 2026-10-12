import { useState } from "react";

import { getCapacityScenario } from "../core/api.js";
import type { CapacityScenario } from "../core/api-types.js";
import type { CapacitySnapshot, FlowDiagnosis } from "../core/generated/runtime-state.js";
import { Meters, serviceCenterRows } from "./fleet.js";
import { formatBps, formatPercent } from "./format.js";
import type { LiveSnapshot } from "./store.js";

const FLOW_BADGE: Record<FlowDiagnosis["status"], string> = {
  healthy: "badge-success",
  saturated: "badge-warning",
  overloaded: "badge-error",
};

const PRIORS = "calibration priors, observe-only";

/**
 * Which service center is closest to saturation, and how a change would
 * move each one. The engine computes every number; this page only shows
 * them and says how sure each is.
 */
export function Capacity({ live }: { live: LiveSnapshot }) {
  const capacity = live.metrics?.capacity ?? null;
  if (capacity === null) {
    return <p className="opacity-70">Waiting for the capacity snapshot…</p>;
  }
  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold">Capacity</h2>
      <div className="grid gap-4 lg:grid-cols-2">
        <section className="card bg-base-200" aria-labelledby="capacity-now">
          <div className="card-body gap-3 p-4">
            <h3 id="capacity-now" className="card-title text-base">
              Service centers now
            </h3>
            <dl className="grid grid-cols-2 gap-2 text-sm sm:grid-cols-4">
              {[
                ["Ingress", `${Math.round(capacity.ingressPps)} pps`],
                ["Egress work", `${Math.round(capacity.egressPps)} pps`],
                ["Media", formatBps(capacity.mediaBps)],
                ["Outputs · stages", `${capacity.activeLeaves} · ${capacity.uniqueStages}`],
              ].map(([label, value]) => (
                <div key={label} className="rounded bg-base-100 p-2">
                  <dt className="text-xs opacity-70">{label}</dt>
                  <dd className="tabular-nums">{value}</dd>
                </div>
              ))}
            </dl>
            <Meters
              title={`Projected load · ${PRIORS} · hottest: ${capacity.hottestCenter}`}
              rows={serviceCenterRows(capacity)}
            />
            <FlowDoctor capacity={capacity} />
          </div>
        </section>
        <Scenario />
      </div>
    </div>
  );
}

function FlowDoctor({ capacity }: { capacity: CapacitySnapshot }) {
  const { flow } = capacity;
  return (
    <section className="rounded bg-base-100 p-3" aria-labelledby="capacity-flow">
      <div className="flex flex-wrap items-center gap-2">
        <h4 id="capacity-flow" className="font-medium">
          Flow Doctor · {flow.center}
        </h4>
        <span className={`badge badge-sm ${FLOW_BADGE[flow.status]}`}>{flow.status}</span>
        <span className="text-xs opacity-70">
          {capacity.flowMeasured
            ? "measured from the busiest egress shard"
            : `no shard runs: projected from ${PRIORS}`}
        </span>
      </div>
      <dl className="mt-2 grid grid-cols-2 gap-2 text-sm sm:grid-cols-4">
        {[
          ["Utilization", formatPercent(flow.utilization * 100)],
          ["Queue", String(Math.round(flow.queue))],
          ["Progress age", `${Math.round(flow.delayMs)} ms`],
          [
            "Deadline slack",
            flow.deadlineSlackMs === null ? "none" : `${Math.round(flow.deadlineSlackMs)} ms`,
          ],
          ["Backlog slope", String(flow.backlogSlope)],
          ["Errors", String(flow.errors)],
        ].map(([label, value]) => (
          <div key={label}>
            <dt className="text-xs opacity-70">{label}</dt>
            <dd className="tabular-nums">{value}</dd>
          </div>
        ))}
      </dl>
    </section>
  );
}

function Scenario() {
  const [addOutputs, setAddOutputs] = useState(100);
  const [addStages, setAddStages] = useState(0);
  const [result, setResult] = useState<CapacityScenario | null>(null);
  return (
    <section className="card bg-base-200" aria-labelledby="capacity-scenario">
      <div className="card-body gap-3 p-4">
        <h3 id="capacity-scenario" className="card-title text-base">
          Scenario
          <span className="text-xs font-normal opacity-70">{PRIORS}</span>
        </h3>
        {/* A failed request shows in the app's error alert. */}
        <form
          className="flex flex-wrap items-end gap-3"
          onSubmit={(event) => {
            event.preventDefault();
            void getCapacityScenario({ addOutputs, addStages }).then(setResult);
          }}
        >
          <label className="form-control">
            <span className="label-text text-xs">More outputs</span>
            <input
              type="number"
              min={0}
              max={100000}
              className="input input-sm input-bordered w-28"
              value={addOutputs}
              onChange={(event) => setAddOutputs(Math.max(0, Number(event.target.value) || 0))}
            />
          </label>
          <label className="form-control">
            <span className="label-text text-xs">More shared stages</span>
            <input
              type="number"
              min={0}
              max={1000}
              className="input input-sm input-bordered w-28"
              value={addStages}
              onChange={(event) => setAddStages(Math.max(0, Number(event.target.value) || 0))}
            />
          </label>
          <button type="submit" className="btn btn-sm btn-primary">
            Project
          </button>
        </form>
        <p className="text-xs opacity-70">
          The model prices outputs and stages, not their protocol or pipeline: it
          multiplies the current ingest by the output count.
        </p>
        {result && (
          <div className="space-y-3" aria-label="Scenario result">
            <p className="text-sm">
              Projected hottest center: <b>{result.projected.hottestCenter}</b> ·{" "}
              {formatPercent(result.current.projectedUtilization * 100)} →{" "}
              <b>{formatPercent(result.projected.projectedUtilization * 100)}</b>{" "}
              <span className={`badge badge-sm ${FLOW_BADGE[result.projected.flow.status]}`}>
                {result.projected.flow.status}
              </span>
            </p>
            <Meters title="Now" rows={serviceCenterRows(result.current)} />
            <Meters
              title={`With ${result.addOutputs} more outputs and ${result.addStages} more stages`}
              rows={serviceCenterRows(result.projected)}
            />
          </div>
        )}
      </div>
    </section>
  );
}
