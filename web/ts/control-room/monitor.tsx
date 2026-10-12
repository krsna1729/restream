import { useEffect, useRef, useState } from "react";

import { getProcessingGraph, getResourceMap } from "../core/api.js";
import type { GraphData, ResourceMapSnapshot } from "../core/api-types.js";
import { formatBps, formatPercent } from "./format.js";
import { MAP_TOP_N } from "./inspect-model.js";
import { appendSample, SIGNALS, sparkline, takeSample } from "./monitor-model.js";
import type { MonitorSample, Signal } from "./monitor-model.js";
import { PipelineScope } from "./pipeline-scope.js";
import { Freshness, usePolled } from "./polled.js";
import type { Polled } from "./polled.js";
import { SAMPLE_INTERVAL_MS, SAMPLE_STALE_AFTER_MS } from "./store.js";
import type { LiveSnapshot } from "./store.js";

const WINDOW_MS = 15 * 60 * 1000;
const PLOT_WIDTH = 600;
const PLOT_HEIGHT = 40;

const SIGNAL_LABELS: Record<Signal, string> = {
  ingestBps: "Ingest rate",
  egressBps: "Egress rate",
  deliveringPercent: "Wanted outputs delivering",
  maxFeedLag: "Worst feed lag",
  ringFillPercent: "Source ring fill",
  maxShardProgressMs: "Slowest shard progress",
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
  const graph = usePolled<GraphData>(() => getProcessingGraph(props.pipelineId));
  const resources = usePolled<ResourceMapSnapshot>(() =>
    getResourceMap(props.pipelineId, { view: "detail", topN: MAP_TOP_N }),
  );
  // The sampler reads the newest sources without restarting its clock.
  const latest = useRef({ live: props.live, graph, resources });
  useEffect(() => {
    latest.current = { live: props.live, graph, resources };
  });
  const [history, setHistory] = useState<readonly MonitorSample[]>([]);
  useEffect(() => {
    const timer = window.setInterval(() => {
      const { live, graph, resources } = latest.current;
      const now = Date.now();
      const ratesFresh =
        live.telemetryAtMs !== null && now - live.telemetryAtMs <= SAMPLE_STALE_AFTER_MS;
      const sample = takeSample({
        atMs: now,
        pipelineId: props.pipelineId,
        // While the state stream is down, its last snapshot is not now.
        outputs: live.stream === "live" ? (live.state?.outputs ?? []) : [],
        rates: ratesFresh ? live.rates : null,
        graph: fresh(graph, now),
        resources: fresh(resources, now),
      });
      setHistory((previous) => appendSample(previous, sample, WINDOW_MS));
    }, SAMPLE_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, [props.pipelineId]);

  const first = history[0];
  const last = history[history.length - 1];
  return (
    <section className="card bg-base-200" aria-labelledby="monitor-signals">
      <div className="card-body gap-3 p-4">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h3 id="monitor-signals" className="card-title text-base">
            Correlated signals
            <span className="text-xs font-normal opacity-70">
              one clock · since this page opened, up to 15 min · {history.length}{" "}
              samples
            </span>
          </h3>
          <span className="flex gap-1">
            <Freshness atMs={graph.atMs} />
            <Freshness atMs={resources.atMs} />
          </span>
        </div>
        {first === undefined || last === undefined ? (
          <p className="opacity-70">Collecting the first sample…</p>
        ) : (
          <dl className="space-y-2">
            {SIGNALS.map((signal) => (
              <div
                key={signal}
                className="grid grid-cols-[12rem_1fr_7rem] items-center gap-3"
              >
                <dt className="text-sm">{SIGNAL_LABELS[signal]}</dt>
                <dd>
                  <svg
                    viewBox={`0 0 ${PLOT_WIDTH} ${PLOT_HEIGHT}`}
                    preserveAspectRatio="none"
                    className="h-10 w-full rounded bg-base-100"
                    role="img"
                    aria-label={`${SIGNAL_LABELS[signal]} over time`}
                  >
                    {sparkline(history, signal, first.atMs, last.atMs, PLOT_WIDTH, PLOT_HEIGHT).map(
                      (points) => (
                        <polyline
                          key={points}
                          points={points}
                          fill="none"
                          stroke="currentColor"
                          strokeWidth="2"
                          strokeLinecap="round"
                          strokeLinejoin="round"
                          vectorEffect="non-scaling-stroke"
                        />
                      ),
                    )}
                  </svg>
                </dd>
                <dd
                  className="text-right text-sm tabular-nums"
                  aria-label={`${SIGNAL_LABELS[signal]} now`}
                >
                  {formatSignal(signal, last[signal])}
                </dd>
              </div>
            ))}
          </dl>
        )}
      </div>
    </section>
  );
}
