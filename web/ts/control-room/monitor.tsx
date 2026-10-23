import { useEffect, useRef, useState } from "react";

import { getResourceMap } from "../core/api.js";
import type { ResourceMapSnapshot } from "../core/api-types.js";
import { formatBps, formatPercent } from "./format.js";
import { MAP_TOP_N } from "./inspect-model.js";
import { incidentOnsets, SIGNALS, takeSample } from "./monitor-model.js";
import type { MonitorSample, Signal } from "./monitor-model.js";
import { MonitorPanels } from "./monitor-panels.js";
import { PipelineScope } from "./pipeline-scope.js";
import { Freshness, usePolled } from "./polled.js";
import type { Polled } from "./polled.js";
import { appendSample, WINDOW_MS } from "./sample-window.js";
import { Panel } from "../ui/panel.js";
import { TimeSeries } from "../ui/time-series.js";
import { StatusBadge } from "../ui/tone.js";
import { pipelineGraph } from "./state-views.js";
import { SAMPLE_INTERVAL_MS, SAMPLE_STALE_AFTER_MS } from "./store.js";
import type { LiveSnapshot } from "./store.js";

const SIGNAL_LABELS: Record<Signal, string> = {
  ingestBps: "Ingest rate",
  egressBps: "Egress rate",
  deliveringPercent: "Wanted outputs delivering",
  maxFeedLag: "Worst feed lag",
  ringFillPercent: "Source ring fill",
  maxShardProgressMs: "Slowest shard progress",
  degradedOutputs: "Degraded outputs",
};

const SIGNAL_COLOR: Record<Signal, string> = {
  ingestBps: "text-primary",
  egressBps: "text-accent",
  deliveringPercent: "text-success",
  maxFeedLag: "text-secondary",
  ringFillPercent: "text-info",
  maxShardProgressMs: "text-secondary",
  degradedOutputs: "text-warning",
};

function formatSignal(signal: Signal, value: number | null): string {
  if (value === null) return "--";
  switch (signal) {
    case "ingestBps":
    case "egressBps":
      return formatBps(value);
    case "deliveringPercent":
    case "ringFillPercent":
      return formatPercent(value);
    case "maxFeedLag":
      return `${value} units`;
    case "maxShardProgressMs":
      return `${value} ms`;
    case "degradedOutputs":
      return String(value);
  }
}

/** Correlated signals of one pipeline on one clock. */
export function Monitor(props: { live: LiveSnapshot; pipelineId: string | null }) {
  return (
    <PipelineScope
      live={props.live}
      workspace="monitor"
      pipelineId={props.pipelineId}
      verb="monitor"
    >
      {(pipelineId) => <Trends live={props.live} pipelineId={pipelineId} />}
    </PipelineScope>
  );
}

/** A polled value only while fresh: a stale one becomes a gap, not a flat line. */
function fresh<T>(polled: Polled<T>, nowMs: number): T | null {
  return polled.atMs !== null && nowMs - polled.atMs <= SAMPLE_STALE_AFTER_MS
    ? polled.value
    : null;
}

function Trends(props: { live: LiveSnapshot; pipelineId: string }) {
  const resources = usePolled<ResourceMapSnapshot>(
    ["resource-map", props.pipelineId, MAP_TOP_N],
    () => getResourceMap(props.pipelineId, { view: "detail", topN: MAP_TOP_N }),
  );
  // The sampler reads the newest sources without restarting its clock.
  const latest = useRef({ live: props.live, resources });
  useEffect(() => {
    latest.current = { live: props.live, resources };
  });
  const [history, setHistory] = useState<readonly MonitorSample[]>([]);
  // Paused: the page shows the window as it was; sampling goes on.
  const [frozen, setFrozen] = useState<readonly MonitorSample[] | null>(null);
  const [comparing, setComparing] = useState(false);
  useEffect(() => {
    const timer = window.setInterval(() => {
      const { live, resources } = latest.current;
      const now = Date.now();
      const telemetryFresh =
        live.telemetryAtMs !== null && now - live.telemetryAtMs <= SAMPLE_STALE_AFTER_MS;
      const sample = takeSample({
        atMs: now,
        pipelineId: props.pipelineId,
        // While the state stream is down, its last snapshot is not now.
        outputs: live.stream === "live" ? (live.state?.outputs ?? []) : null,
        rates: telemetryFresh ? live.rates : null,
        // The ring fill and feed lag it reads are the telemetry sample's.
        graph: telemetryFresh
          ? pipelineGraph(live.state, live.telemetry, props.pipelineId)
          : null,
        resources: fresh(resources, now),
      });
      setHistory((previous) => appendSample(previous, sample, WINDOW_MS));
    }, SAMPLE_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [props.pipelineId]);

  const shown = frozen ?? history;
  const compare = frozen && comparing ? history : undefined;
  const onsets = incidentOnsets(shown);
  const first = shown[0];
  const last = shown[shown.length - 1];
  return (
    <div className="flex flex-col gap-4">
      <Panel
        id="monitor-signals"
        title="Correlated signals"
        subtitle={`one clock · since this page opened, up to 15 min · ${shown.length} samples${frozen && last ? ` · paused at ${new Date(last.atMs).toLocaleTimeString()}` : ""}${compare ? " · dashed: now" : ""}`}
        actions={
          <>
            <button
              type="button"
              className={`btn btn-sm ${frozen ? "btn-primary" : ""}`}
              aria-pressed={frozen !== null}
              onClick={() => {
                setFrozen(frozen ? null : history);
                setComparing(false);
              }}
            >
              {frozen ? "Resume" : "Pause"}
            </button>
            <button
              type="button"
              className="btn btn-sm"
              aria-pressed={comparing}
              disabled={frozen === null}
              title="Overlay the live window on the paused one"
              onClick={() => setComparing(!comparing)}
            >
              Compare
            </button>
            <Freshness atMs={props.live.telemetryAtMs} />
            <Freshness atMs={resources.atMs} />
          </>
        }
      >
        {onsets.length > 0 && (
          <p className="mb-3 flex flex-wrap items-center gap-2 text-sm" aria-label="Incident onsets">
            <StatusBadge tone="warn" dot>
              incident
            </StatusBadge>
            {onsets.length === 1 ? "An output degraded at " : "Outputs degraded at "}
            {onsets.map((at) => new Date(at).toLocaleTimeString()).join(", ")}; the cursor marks it on every signal.
          </p>
        )}
        {first === undefined || last === undefined ? (
          <p className="text-base-content/70">Collecting the first sample…</p>
        ) : (
          <dl className="grid gap-3 md:grid-cols-2 2xl:grid-cols-3">
            {SIGNALS.map((signal) => (
              <div key={signal} className="min-w-0 rounded-box border border-base-content/10 bg-base-100 p-3">
                <div className="mb-1 flex items-baseline justify-between gap-2">
                  <dt className="kicker truncate">{SIGNAL_LABELS[signal]}</dt>
                  <dd className="tabular text-lg font-semibold" aria-label={`${SIGNAL_LABELS[signal]} now`}>
                    {formatSignal(signal, last[signal])}
                  </dd>
                </div>
                <dd>
                  <TimeSeries
                    samples={shown}
                    label={`${SIGNAL_LABELS[signal]} over time`}
                    height={72}
                    showLegend={false}
                    top={signal === "deliveringPercent" || signal === "ringFillPercent" ? 100 : undefined}
                    ghost={compare}
                    axisFormat={signal === "ingestBps" || signal === "egressBps" ? (value) => formatBps(value).replace("/s", "") : undefined}
                    marks={onsets.map((atMs) => ({ atMs, tone: "warn" as const, kind: "incident" }))}
                    series={[{ id: signal, label: SIGNAL_LABELS[signal], pick: (sample) => sample[signal], color: SIGNAL_COLOR[signal], area: true, format: (value) => formatSignal(signal, value) }]}
                  />
                </dd>
              </div>
            ))}
          </dl>
        )}
      </Panel>
      <MonitorPanels live={props.live} pipelineId={props.pipelineId} />
    </div>
  );
}
