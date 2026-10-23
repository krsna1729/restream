import { useEffect, useState } from "react";

import { getCapacityScenario } from "../core/api.js";
import type {
  CapacityCalibration,
  CapacityLimits,
  CapacityScenario,
  OutputProtocol,
} from "../core/api-types.js";
import type { CapacitySnapshot, FlowDiagnosis } from "../core/generated/runtime-state.js";
import { Meters, serviceCenterRows } from "./fleet.js";
import { formatBps, formatPercent } from "./format.js";
import { PlaceLink } from "./router.js";
import type { LiveSnapshot } from "./store.js";
import { Kpi, KeyValue, KpiRow } from "../ui/kpi.js";
import { PageHeader } from "../ui/page-header.js";
import { Panel } from "../ui/panel.js";
import { StatusBadge } from "../ui/tone.js";
import type { Tone } from "../ui/tone.js";

const FLOW_TONE: Record<FlowDiagnosis["status"], Tone> = {
  healthy: "ok",
  saturated: "warn",
  overloaded: "bad",
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
    return <p className="text-sm text-base-content/70">Waiting for the capacity snapshot…</p>;
  }
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Capacity · observe-only"
        title="How much more this host can carry"
        subtitle="Load per service center from the calibration priors, the measured egress flow, and what-if scenarios priced by the engine."
      />
      <div className="grid gap-4 lg:grid-cols-2">
        <Panel id="capacity-now" title="Service centers now">
          <div className="flex flex-col gap-3">
            <KpiRow>
              <Kpi label="Ingress" value={`${Math.round(capacity.ingressPps)} pps`} />
              <Kpi label="Egress work" value={`${Math.round(capacity.egressPps)} pps`} />
              <Kpi label="Media" value={formatBps(capacity.mediaBps)} />
              <Kpi label="Outputs · stages" value={`${capacity.activeLeaves} · ${capacity.uniqueStages}`} />
            </KpiRow>
            <Meters
              title={`Projected load · ${PRIORS} · hottest: ${capacity.hottestCenter}`}
              rows={serviceCenterRows(capacity)}
            />
            <FlowDoctor capacity={capacity} />
          </div>
        </Panel>
        <Scenario live={live} />
      </div>
    </div>
  );
}

function FlowDoctor({ capacity }: { capacity: CapacitySnapshot }) {
  const { flow } = capacity;
  return (
    <section className="rounded-box border border-base-content/10 bg-base-100 p-3" aria-labelledby="capacity-flow">
      <div className="flex flex-wrap items-center gap-2">
        <h4 id="capacity-flow" className="font-medium">
          Flow Doctor · {flow.center}
        </h4>
        <StatusBadge tone={FLOW_TONE[flow.status]} dot>
          {flow.status}
        </StatusBadge>
        <span className="text-xs text-base-content/70">
          {capacity.flowMeasured
            ? "measured from the busiest egress shard"
            : `no shard runs: projected from ${PRIORS}`}
        </span>
      </div>
      <div className="mt-2 grid gap-x-6 gap-y-1.5 sm:grid-cols-2">
        <KeyValue
          rows={[
            ["Utilization", formatPercent(flow.utilization * 100)],
            ["Queue", String(Math.round(flow.queue))],
            ["Progress age", `${Math.round(flow.delayMs)} ms`],
          ]}
        />
        <KeyValue
          rows={[
            ["Deadline slack", flow.deadlineSlackMs === null ? "none" : `${Math.round(flow.deadlineSlackMs)} ms`],
            ["Backlog slope", String(flow.backlogSlope)],
            ["Errors", String(flow.errors)],
          ]}
        />
      </div>
    </section>
  );
}

const PROTOCOLS: ReadonlyArray<[OutputProtocol, string]> = [
  ["rtmp", "RTMP"],
  ["rtmps", "RTMPS (TLS cost not priced)"],
  ["srt", "SRT (2× RTMP: shard prior at 4.8 Mbit/s)"],
];

// Each new rendition is one more FFmpeg stage its outputs share.
const ENCODINGS: ReadonlyArray<[number, string]> = [
  [0, "Source passthrough"],
  [1, "1 new rendition"],
  [2, "2 new renditions"],
  [3, "3 new renditions"],
];

const LIMIT_LABELS: ReadonlyArray<[keyof CapacityLimits, string, (value: number) => string]> = [
  ["ingressPps", "Ingress", (value) => `${Math.round(value)} pps`],
  ["egressPps", "Egress per shard", (value) => `${Math.round(value)} pps`],
  ["nicBps", "NIC", formatBps],
  ["memoryBytes", "Memory", (value) => `${Math.round(value / 1_000_000)} MB`],
  ["ffmpegStages", "FFmpeg stages", (value) => String(value)],
  ["diskBps", "Disk", formatBps],
];

function Scenario({ live }: { live: LiveSnapshot }) {
  const [addOutputs, setAddOutputs] = useState(100);
  const [addStages, setAddStages] = useState(0);
  const [protocol, setProtocol] = useState<OutputProtocol>("rtmp");
  const [pipelineId, setPipelineId] = useState("");
  const [result, setResult] = useState<CapacityScenario | null>(null);
  const project = () =>
    void getCapacityScenario({
      addOutputs: { [protocol]: addOutputs },
      addStages,
      pipelineId: pipelineId || undefined,
    }).then((scenario) => {
      if (scenario) setResult(scenario);
    });
  // The default scenario once, on arrival: the calibration shows without a
  // click. Later changes project on submit only.
  useEffect(project, []);
  const pipelines = live.state?.pipelines ?? [];
  const name = (id: string) => pipelines.find((pipeline) => pipeline.id === id)?.name ?? id;
  return (
    <Panel id="capacity-scenario" title="Scenario" subtitle={PRIORS}>
      <div className="flex flex-col gap-3">
        {/* A failed request (a pipeline with no live ingest) shows in the
            app's error alert. */}
        <form
          className="flex flex-wrap items-end gap-3"
          onSubmit={(event) => {
            event.preventDefault();
            project();
          }}
        >
          <label className="form-control">
            <span className="label-text text-xs">More outputs</span>
            <input
              type="number"
              min={0}
              max={100000}
              className="input input-sm w-28"
              value={addOutputs}
              onChange={(event) => setAddOutputs(Math.max(0, Number(event.target.value) || 0))}
            />
          </label>
          <label className="form-control">
            <span className="label-text text-xs">Carrying the ingest of</span>
            <select
              className="select select-sm"
              value={pipelineId}
              onChange={(event) => setPipelineId(event.target.value)}
            >
              <option value="">The highest-bitrate live pipeline</option>
              {pipelines.map((pipeline) => (
                <option key={pipeline.id} value={pipeline.id}>
                  {pipeline.name}
                </option>
              ))}
            </select>
          </label>
          <label className="form-control">
            <span className="label-text text-xs">Protocol</span>
            <select
              className="select select-sm"
              value={protocol}
              onChange={(event) => setProtocol(event.target.value as OutputProtocol)}
            >
              {PROTOCOLS.map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </select>
          </label>
          <label className="form-control">
            <span className="label-text text-xs">Encoding</span>
            <select
              className="select select-sm"
              value={addStages}
              onChange={(event) => setAddStages(Number(event.target.value))}
            >
              {ENCODINGS.map(([stages, label]) => (
                <option key={stages} value={stages}>
                  {label}
                </option>
              ))}
            </select>
          </label>
          <button type="submit" className="btn btn-sm btn-primary">
            Project
          </button>
        </form>
        <p className="text-xs text-base-content/70">
          Both sides are priced on the priors, so they compare like with like;
          the measured flow is the Flow Doctor's. Each added output carries its
          pipeline's ingest, in the packets its protocol sends; a rendition is
          one FFmpeg stage its outputs share. To price a planned change with its
          topology diff,{" "}
          <PlaceLink place={{ workspace: "changes", pipelineId: pipelineId || null }} className="link">
            draft it in Changes
          </PlaceLink>
          .
        </p>
        {result && (
          <div className="space-y-3" aria-label="Scenario result">
            <p className="text-sm">
              Projected hottest center: <b>{result.projected.hottestCenter}</b> ·{" "}
              {formatPercent(result.baseline.projectedUtilization * 100)} →{" "}
              <b>{formatPercent(result.projected.projectedUtilization * 100)}</b>{" "}
              <StatusBadge tone={FLOW_TONE[result.projected.flow.status]} dot>
                {result.projected.flow.status}
              </StatusBadge>
            </p>
            <p className="text-xs text-base-content/70">
              {result.ratedBy === null
                ? "No ingest is live: the added outputs carry nothing yet."
                : `Each added output carries ${name(result.ratedBy)}: ${formatBps(result.ingest.bps)}, ${Math.round(result.ingest.pps)} pps.`}
            </p>
            <Meters title="Now, on the priors" rows={serviceCenterRows(result.baseline)} />
            <Meters
              title={`With ${addedOutputs(result.addOutputs)} and ${result.addStages} more stages`}
              rows={serviceCenterRows(result.projected)}
            />
            <Calibration calibration={result.calibration} />
          </div>
        )}
      </div>
    </Panel>
  );
}

function addedOutputs(added: CapacityScenario["addOutputs"]): string {
  const parts = PROTOCOLS.filter(([protocol]) => added[protocol] > 0).map(
    ([protocol]) => `${added[protocol]} more ${protocol.toUpperCase()}`,
  );
  return parts.length === 0 ? "no more outputs" : `${parts.join(", ")} outputs`;
}

/** What every projection is priced on, and where each number came from. */
function Calibration({ calibration }: { calibration: CapacityCalibration }) {
  return (
    <section className="rounded-box border border-base-content/10 bg-base-100 p-3" aria-labelledby="capacity-calibration">
      <h4 id="capacity-calibration" className="font-medium">
        Calibration
        <span className="ml-2 text-xs font-normal text-base-content/70">
          model v{calibration.modelVersion} · safe below{" "}
          {formatPercent(calibration.safeUtilization * 100)}
        </span>
      </h4>
      <table className="table table-xs mt-1" aria-label="Capacity limits">
        <thead>
          <tr>
            <th>Center</th>
            <th>Limit</th>
            <th>Source</th>
          </tr>
        </thead>
        <tbody>
          {LIMIT_LABELS.map(([key, label, format]) => (
            <tr key={key}>
              <td>{label}</td>
              <td className="tabular-nums">{format(calibration.limits[key])}</td>
              <td>
                {calibration.sources[key] === "configured" ? (
                  <StatusBadge tone="info">configured</StatusBadge>
                ) : (
                  <StatusBadge
                    tone="neutral"
                    title={`A conservative prior scaled to ${calibration.priorCpus} CPUs, not a measurement of this host`}
                  >
                    prior · {calibration.priorCpus} CPUs
                  </StatusBadge>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}
