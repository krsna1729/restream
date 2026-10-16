import { useEffect, useState } from "react";

import { getCapacityScenario } from "../core/api.js";
import type { CapacityScenario, ChangeOperation, TopologyDiff } from "../core/api-types.js";
import { formatPercent } from "./format.js";
import { Meters, serviceCenterRows } from "./fleet.js";
import { convergenceTimeline, liveConvergence, scenarioLoad, topologyLines } from "./changes-model.js";
import type { LiveSnapshot } from "./store.js";

/** The change's topology and what it costs, before anything is applied. */
export function Impact({ topology }: { topology: TopologyDiff }) {
  const load = scenarioLoad(topology);
  const [scenario, setScenario] = useState<CapacityScenario | null>(null);
  const adds = load.addOutputs > 0 || load.addStages > 0;
  useEffect(() => {
    setScenario(null);
    if (!adds) return;
    let current = true;
    void getCapacityScenario({ addOutputs: load.addOutputs, addStages: load.addStages }).then((result) => {
      if (current) setScenario(result);
    });
    return () => {
      current = false;
    };
    // Re-price only when one of the numbers changes, not on every render.
  }, [adds, load.addOutputs, load.addStages]);
  const lines = topologyLines(topology);
  return (
    <section className="card bg-base-200" aria-labelledby="changes-impact">
      <div className="card-body gap-3 p-4">
        <h3 id="changes-impact" className="card-title text-base">
          Impact
          <span className="text-xs font-normal opacity-70">
            outputs wanted running, planned for{" "}
            {topology.ingestCodec ? `the live ${topology.ingestCodec} ingest` : "no live ingest (no codec bridge)"}
          </span>
        </h3>
        <dl className="grid grid-cols-2 gap-2 text-sm md:grid-cols-4">
          <div>
            <dt className="opacity-70">Outputs added</dt>
            <dd className="tabular-nums">{load.addOutputs}</dd>
          </div>
          <div>
            <dt className="opacity-70">Outputs removed</dt>
            <dd className="tabular-nums">{load.removedOutputs}</dd>
          </div>
          <div>
            <dt className="opacity-70">New stages</dt>
            <dd className="tabular-nums">{topology.addedStages.length}</dd>
          </div>
          <div>
            <dt className="opacity-70">Retired stages</dt>
            <dd className="tabular-nums">{topology.retiredStages.length}</dd>
          </div>
        </dl>
        {lines.length === 0 ? (
          <p className="text-sm opacity-70">No output is wanted running before or after.</p>
        ) : (
          <table className="table table-sm" aria-label="Topology diff">
            <thead>
              <tr>
                <th>Stage</th>
                <th>Protocol</th>
                <th className="text-right">Before</th>
                <th className="text-right">After</th>
              </tr>
            </thead>
            <tbody>
              {lines.map((line) => (
                <tr key={`${line.stage} ${line.protocol}`} data-changed={line.before !== line.after}>
                  <td className="font-mono text-xs">
                    {line.stage}{" "}
                    {line.newStage ? (
                      <span className="badge badge-warning badge-sm">new stage</span>
                    ) : line.after > line.before && line.before > 0 ? (
                      <span className="badge badge-success badge-sm">reuse</span>
                    ) : null}
                  </td>
                  <td>{line.protocol.toUpperCase()}</td>
                  <td className="text-right tabular-nums">{line.before}</td>
                  <td className={`text-right tabular-nums ${line.before !== line.after ? "font-semibold" : ""}`}>
                    {line.after}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {adds ? (
          scenario && (
            <div className="space-y-2" aria-label="Projected load">
              <p className="text-sm">
                Hottest center <b>{scenario.projected.hottestCenter}</b>:{" "}
                {formatPercent(scenario.baseline.projectedUtilization * 100)} →{" "}
                <b>{formatPercent(scenario.projected.projectedUtilization * 100)}</b>{" "}
                {scenario.projected.flow.status === "healthy" ? (
                  <span className="badge badge-success badge-sm">within headroom</span>
                ) : (
                  <span className="badge badge-warning badge-sm">{scenario.projected.flow.status}</span>
                )}
              </p>
              <div className="grid gap-3 md:grid-cols-2">
                <Meters title="Before, on the priors" rows={serviceCenterRows(scenario.baseline)} />
                <Meters title="After, on the priors" rows={serviceCenterRows(scenario.projected)} />
              </div>
              <p className="text-xs opacity-70">
                The capacity scenario prices added outputs and stages on the calibration priors;
                it does not credit removals.
              </p>
            </div>
          )
        ) : (
          <p className="text-sm opacity-70">The change adds no outputs or stages, so it adds no load.</p>
        )}
      </div>
    </section>
  );
}

function offset(ms: number): string {
  return ms < 1000 ? `+${ms} ms` : `+${(ms / 1000).toFixed(1)} s`;
}

/** The operation from creation to now: its audit events, each output change
 * the apply made, and where those outputs stand in the live state. */
export function Convergence(props: { operation: ChangeOperation; live: LiveSnapshot }) {
  const steps = convergenceTimeline(props.operation);
  const now = liveConvergence(props.operation, props.live.state?.outputs ?? []);
  return (
    <div className="space-y-1">
      <h4 className="text-sm font-semibold">Convergence</h4>
      <ol className="text-xs" aria-label="Convergence timeline">
        {steps.map((step, index) => (
          <li key={index} className="grid grid-cols-[5rem_1fr] gap-2">
            <span className="tabular-nums opacity-70">{offset(step.offsetMs)}</span>
            <span>{step.label}</span>
          </li>
        ))}
      </ol>
      {now.total > 0 && (
        <p className="text-sm" aria-label="Live convergence">
          Now: {now.converged} of {now.total} changed outputs converged
          {now.states
            .filter(([, state]) => state !== "converged")
            .map(([name, state]) => ` · ${name} ${state}`)
            .join("")}
        </p>
      )}
    </div>
  );
}
