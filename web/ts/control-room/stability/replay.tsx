// Replay a pipeline: the flight recorder's history as one story over any
// span it keeps. An overview of the whole history sits on top; drag across
// it, pick a preset, or widen and narrow the span with the zoom buttons.
// The span lives in the URL, so an incident links straight to its story.

import { useMemo, useRef, useState } from "react";
import type { PointerEvent } from "react";

import { getFlightRecorder } from "../../core/api.js";
import type { FlightTrace } from "../../core/api-types.js";
import { EmptyState, Panel } from "../../ui/panel.js";
import { TimeSeries } from "../../ui/time-series.js";
import { usePolled } from "../polled.js";
import type { LiveSnapshot } from "../store.js";
import { useClock } from "../when-view.js";
import { absoluteTime } from "../when.js";
import { PRESETS, clampWindow, edges, fromDrag, lastSpan, pointsFor, windowLabel, zoom } from "./replay-window.js";
import type { ReplayWindow } from "./replay-window.js";
import { Player } from "./player.js";
import { storyFromTrace } from "./story-model.js";

const DAY_MS = 24 * 60 * 60_000;

export function Replay(props: {
  live: LiveSnapshot;
  pipelineId: string | null;
  window: ReplayWindow;
  onChange: (pipelineId: string, window: ReplayWindow) => void;
}) {
  if (!props.pipelineId) return <EmptyState title="No pipeline to replay" detail="Create a pipeline; the flight recorder keeps its history from then." />;
  const pipelineId = props.pipelineId;
  return (
    <>
      <div className="flex flex-wrap items-center gap-3">
        <label className="flex items-center gap-2 text-sm">
          Pipeline
          <select
            className="select select-sm w-64"
            value={pipelineId}
            onChange={(event) => props.onChange(event.target.value, props.window)}
          >
            {(props.live.state?.pipelines ?? []).map((pipeline) => (
              <option key={pipeline.id} value={pipeline.id}>
                {pipeline.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      <ReplayPlayer key={pipelineId} pipelineId={pipelineId} window={props.window} onWindow={(window) => props.onChange(pipelineId, window)} />
    </>
  );
}

function ReplayPlayer(props: { pipelineId: string; window: ReplayWindow; onWindow: (window: ReplayWindow) => void }) {
  const clockMs = useClock();
  const nowMs = Math.max(clockMs, Date.now());
  // The whole history, coarse, for the overview (a minute fresh is enough).
  const overview = usePolled(
    ["flight-overview", props.pipelineId],
    () => getFlightRecorder(props.pipelineId, new Date(Date.now() - DAY_MS), { points: 360 }),
    60_000,
  );
  // A span may reach back before the history starts (that part is empty),
  // never past the day the history can hold.
  const oldestMs = Math.min(overview.value?.completeFromMs ?? nowMs, nowMs - DAY_MS);
  const window = clampWindow(props.window, oldestMs, nowMs);
  const { fromMs, toMs } = edges(window, nowMs);
  const following = window.toMs === null;
  // The chosen span at full detail; a following one refreshes, a past one is fixed.
  const trace = usePolled(
    ["flight-window", props.pipelineId, Math.round(window.fromMs / 1000), window.toMs === null ? "now" : Math.round(window.toMs / 1000)],
    () =>
      getFlightRecorder(props.pipelineId, new Date(window.fromMs), {
        until: window.toMs === null ? undefined : new Date(window.toMs),
        points: pointsFor(window, Date.now()),
      }),
    following ? 4_000 : 3_600_000,
  );
  const frames = useMemo(() => (trace.value ? storyFromTrace(trace.value) : []), [trace.value]);
  const set = (next: ReplayWindow) => props.onWindow(clampWindow(next, oldestMs, Date.now()));

  return (
    <>
      <Panel
        id="replay-window"
        title="History"
        subtitle={windowLabel(window, nowMs)}
        actions={
          <div className="flex flex-wrap items-center gap-1.5" role="group" aria-label="Replay span">
            {PRESETS.map(([label, span]) => (
              <button
                key={label}
                type="button"
                className={`btn btn-xs ${following && Math.abs(toMs - fromMs - span) < 60_000 ? "btn-primary" : "btn-outline border-base-content/20"}`}
                onClick={() => set(lastSpan(span, Date.now()))}
              >
                Last {label}
              </button>
            ))}
            <button type="button" className="btn btn-xs btn-outline border-base-content/20" aria-label="Show a longer span" title="Widen (twice the span)" onClick={() => set(zoom(window, 2, oldestMs, Date.now()))}>
              −
            </button>
            <button type="button" className="btn btn-xs btn-outline border-base-content/20" aria-label="Show a shorter span" title="Narrow (half the span)" onClick={() => set(zoom(window, 0.5, oldestMs, Date.now()))}>
              +
            </button>
          </div>
        }
      >
        <Overview trace={overview.value} fromMs={fromMs} toMs={toMs} nowMs={nowMs} onPick={set} />
      </Panel>
      {trace.value === null ? (
        <EmptyState title="Reading the flight recorder…" />
      ) : frames.length < 2 ? (
        <EmptyState title="Nothing recorded in this span" detail="Widen it, or pick a span the history holds." />
      ) : (
        <Player
          key={`${window.fromMs}:${window.toMs ?? "now"}`}
          frames={frames}
          measured
          follow={following}
          title="pipeline"
          footer={footer(trace.value)}
        />
      )}
    </>
  );
}

/** Which samples the story is made of, in words. */
function footer(trace: FlightTrace): string {
  const detailed = trace.detailedFromMs ?? trace.completeFromMs;
  // Only samples actually shown decide it: a span reaching back before the
  // recorder started holds no older, coarser ones.
  if ((trace.samples[0]?.atMs ?? detailed) >= detailed) return "restream flight recorder · measured every 2 s, modelled in between";
  return `restream flight recorder · measured every ${Math.round((trace.persistedIntervalMs ?? 10_000) / 1000)} s before ${absoluteTime(detailed, Date.now())}, every 2 s after, modelled in between`;
}

/** The whole history as one strip; drag across it to choose a span. */
function Overview(props: {
  trace: FlightTrace | null;
  fromMs: number;
  toMs: number;
  nowMs: number;
  onPick: (window: ReplayWindow) => void;
}) {
  const box = useRef<HTMLDivElement | null>(null);
  const [drag, setDrag] = useState<{ a: number; b: number } | null>(null);
  const startMs = props.trace?.completeFromMs ?? props.nowMs - 10 * 60_000;
  const span = Math.max(1, props.nowMs - startMs);
  const samples = useMemo(
    () =>
      (props.trace?.samples ?? []).map((sample) => ({
        atMs: sample.atMs,
        share: sample.outputs.wanted === 0 ? null : (100 * sample.outputs.delivering) / sample.outputs.wanted,
      })),
    [props.trace],
  );
  const at = (event: PointerEvent<HTMLDivElement>) => {
    const rect = box.current?.getBoundingClientRect();
    if (!rect || rect.width === 0) return 0;
    return Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
  };
  const left = drag ? Math.min(drag.a, drag.b) : (props.fromMs - startMs) / span;
  const right = drag ? Math.max(drag.a, drag.b) : (props.toMs - startMs) / span;
  return (
    <div className="flex flex-col gap-1">
      <div className="relative">
        <TimeSeries
          samples={samples}
          label="Share of wanted destinations delivering over the kept history"
          height={56}
          top={100}
          fromMs={startMs}
          toMs={props.nowMs}
          showLegend={false}
          axisFormat={(value) => `${Math.round(value)}%`}
          series={[{ id: "share", label: "Delivering", pick: (sample) => sample.share, color: "text-success", area: true, format: (value) => `${Math.round(value)}%` }]}
        />
        {/* The drag surface sits over the plot (right of the 3.5rem axis). */}
        <div
          ref={box}
          role="slider"
          tabIndex={0}
          aria-label="Choose the replay span: drag across the history, or move it with the arrow keys"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(right * 100)}
          className="absolute inset-y-0 right-0 left-[3.5rem] cursor-crosshair touch-none"
          onKeyDown={(event) => {
            // Arrows move the span by a tenth of itself; the edges stay in the history.
            if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
            event.preventDefault();
            const shift = ((props.toMs - props.fromMs) / 10) * (event.key === "ArrowLeft" ? -1 : 1);
            const toMs = props.toMs + shift;
            props.onPick(toMs >= Date.now() ? { fromMs: props.fromMs + shift, toMs: null } : { fromMs: props.fromMs + shift, toMs });
          }}
          onPointerDown={(event) => {
            event.currentTarget.setPointerCapture(event.pointerId);
            const point = at(event);
            setDrag({ a: point, b: point });
          }}
          onPointerMove={(event) => drag && setDrag({ a: drag.a, b: at(event) })}
          onPointerUp={(event) => {
            if (!drag) return;
            const b = at(event);
            setDrag(null);
            props.onPick(fromDrag(drag.a, b, { fromMs: startMs, toMs: props.nowMs }, Date.now()));
          }}
        >
          <div
            aria-hidden="true"
            className="pointer-events-none absolute inset-y-0 rounded-sm border-x-2 border-primary bg-primary/15"
            style={{ left: `${Math.max(0, left) * 100}%`, width: `${Math.max(0.4, (Math.min(1, right) - Math.max(0, left)) * 100)}%` }}
          />
        </div>
      </div>
      <div className="flex justify-between pl-[3.5rem] text-xs text-base-content/60 tabular">
        <span>{absoluteTime(startMs, props.nowMs)}</span>
        <span>now</span>
      </div>
    </div>
  );
}
