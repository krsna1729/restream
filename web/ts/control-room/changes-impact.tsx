import { useEffect, useState } from "react";

import { getCapacityScenario } from "../core/api.js";
import type { CapacityScenario, ChangeOperation, TopologyDiff } from "../core/api-types.js";
import { Kpi, KpiRow } from "../ui/kpi.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import { formatPercent } from "./format.js";
import { Meters, serviceCenterRows } from "./fleet.js";
import { convergenceTimeline, liveConvergence, scenarioLoad, topologyLines } from "./changes-model.js";
import type { LiveSnapshot } from "./store.js";
import { absoluteTime, fullTime } from "./when.js";

/** The change's topology and what it costs, before anything is applied. */
export function Impact({ topology, pipelineId }: { topology: TopologyDiff; pipelineId: string }) {
  const load = scenarioLoad(topology);
  const [scenario, setScenario] = useState<CapacityScenario | null>(null);
  const adds = load.addOutputs > 0 || load.addStages > 0;
  // A codec means the plan's pipeline is live: price its own ingest.
  const live = topology.ingestCodec !== null;
  const { rtmp, srt } = load.addByProtocol;
  useEffect(() => {
    setScenario(null);
    if (!adds) return;
    let current = true;
    const change = { addOutputs: { rtmp, srt }, addStages: load.addStages };
    void getCapacityScenario(live ? { ...change, pipelineId, quietWhenNotLive: true } : change)
      // The ingest may have ended since the plan: price it unnamed then.
      .then((result) => result ?? (live ? getCapacityScenario(change) : null))
      .then((result) => {
        if (current) setScenario(result);
      });
    return () => {
      current = false;
    };
    // Re-price only when one of the numbers changes, not on every render.
  }, [adds, rtmp, srt, load.addStages, live, pipelineId]);
  const lines = topologyLines(topology);
  return (
    <Panel
      id="changes-impact"
      title="Impact"
      subtitle={`outputs wanted running, planned for ${topology.ingestCodec ? `the live ${topology.ingestCodec} ingest` : "no live ingest (no codec bridge)"}`}
    >
      <div className="flex flex-col gap-3">
        <KpiRow>
          <Kpi label="Outputs added" value={load.addOutputs} />
          <Kpi label="Outputs removed" value={load.removedOutputs} />
          <Kpi label="New stages" value={topology.addedStages.length} />
          <Kpi label="Retired stages" value={topology.retiredStages.length} />
        </KpiRow>
        {lines.length === 0 ? (
          <p className="text-sm text-base-content/70">No output is wanted running before or after.</p>
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
                      <StatusBadge tone="warn">new stage</StatusBadge>
                    ) : line.after > line.before && line.before > 0 ? (
                      <StatusBadge tone="ok">reuse</StatusBadge>
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
                  <StatusBadge tone="ok" dot>
                    within headroom
                  </StatusBadge>
                ) : (
                  <StatusBadge tone="warn" dot>
                    {scenario.projected.flow.status}
                  </StatusBadge>
                )}
              </p>
              <div className="grid gap-3 md:grid-cols-2">
                <Meters title="Before, on the priors" rows={serviceCenterRows(scenario.baseline)} />
                <Meters title="After, on the priors" rows={serviceCenterRows(scenario.projected)} />
              </div>
              <p className="text-xs text-base-content/70">
                The capacity scenario prices added outputs and stages on the calibration priors;
                it does not credit removals.
              </p>
            </div>
          )
        ) : (
          <p className="text-sm text-base-content/70">The change adds no outputs or stages, so it adds no load.</p>
        )}
      </div>
    </Panel>
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
            {/* The offset reads the sequence; the wall-clock time stays on hover
                and on the first step, so a later reader sees when it ran. */}
            <span
              className="tabular-nums text-base-content/70"
              title={fullTime(step.atMs)}
            >
              {index === 0 ? absoluteTime(step.atMs, Date.now(), { seconds: true, noZone: true }) : offset(step.offsetMs)}
            </span>
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
