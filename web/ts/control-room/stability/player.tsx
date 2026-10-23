// The story player both Stability Lab modes share: the destinations now,
// the queues as cells, the KPIs, the chart and the narration, with play,
// pause, speed and a scrub bar.

import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import { ChipGroup } from "../../ui/controls.js";
import { Icon } from "../../ui/icons.js";
import { Meter } from "../../ui/kpi.js";
import { Panel } from "../../ui/panel.js";
import { TimeSeries } from "../../ui/time-series.js";
import type { Band, Mark } from "../../ui/time-series.js";
import { StatusBadge } from "../../ui/tone.js";
import { absoluteTime } from "../when.js";
import { FRAME_MS } from "./sim-engine.js";
import type { CellKind, StoryFrame } from "./story-model.js";

const CELL_CLASS: Record<CellKind, string> = {
  fresh: "bg-primary",
  copy: "bg-error",
  handshake: "bg-base-content/45",
  empty: "bg-base-content/8",
};

const SPEEDS = [1, 2, 4, 10] as const;

function clock(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

export function Player(props: {
  frames: readonly StoryFrame[];
  footer: string;
  title: string;
  /** Live replay: stay at the newest frame unless the operator scrubs. */
  follow?: boolean;
  /** A measured replay: frames are samples of any spacing, played at a
   * steady rate, and the clock reads the time of day. */
  measured?: boolean;
  actions?: (atMs: number) => ReactNode;
  marks?: readonly Mark[];
}) {
  const { frames } = props;
  const [position, setPosition] = useState(props.follow ? frames.length - 1 : 0);
  const [playing, setPlaying] = useState(!props.follow && !props.measured);
  const [speed, setSpeed] = useState<(typeof SPEEDS)[number]>(4);
  const last = frames.length - 1;
  // A measured window of any length plays in about a minute at 4x.
  const step = props.measured ? 200 : props.follow ? (frames[1]?.atMs ?? 2000) - (frames[0]?.atMs ?? 0) : FRAME_MS;
  const raf = useRef<number | null>(null);
  useEffect(() => {
    if (!playing) return;
    let previous = performance.now();
    const tick = (now: number) => {
      const elapsed = now - previous;
      previous = now;
      setPosition((current) => {
        const next = current + (elapsed * speed) / step;
        if (next >= last) {
          setPlaying(false);
          return last;
        }
        return next;
      });
      raf.current = requestAnimationFrame(tick);
    };
    raf.current = requestAnimationFrame(tick);
    return () => {
      if (raf.current !== null) cancelAnimationFrame(raf.current);
    };
  }, [playing, speed, last, step]);
  useEffect(() => {
    if (props.follow && !playing) setPosition(last);
  }, [props.follow, last, playing]);

  const index = Math.max(0, Math.min(last, Math.floor(position)));
  const frame = frames[index];
  const shown = frames.slice(0, index + 1);
  const startMs = frames[0]?.atMs ?? 0;
  const share = frame.wanted === 0 ? 1 : frame.delivering / frame.wanted;
  const waiting = frame.wanted - frame.delivering;
  const bands = useMemo<Band[]>(() => {
    const out: Band[] = [];
    let open: { from: number; tone: Band["tone"] } | null = null;
    for (const candidate of frames) {
      const tone: Band["tone"] | null = candidate.trigger ? "warn" : candidate.chapter.id === "stuck" || candidate.chapter.id === "storm" ? "bad" : null;
      if (open && open.tone !== tone) {
        out.push({ fromMs: open.from, toMs: candidate.atMs, tone: open.tone });
        open = null;
      }
      if (!open && tone) open = { from: candidate.atMs, tone };
    }
    if (open) out.push({ fromMs: open.from, toMs: frames[last].atMs, tone: open.tone });
    return out;
  }, [frames, last]);
  const stalled = share < 0.05 && frame.wanted > 0;
  const chapterTone = frame.chapter.tone === "ok" ? "ok" : frame.chapter.tone === "info" ? "info" : frame.chapter.tone === "warn" ? "warn" : "bad";

  return (
    <div className="flex w-full flex-col gap-3" aria-label="Stability story">
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" className="btn btn-primary btn-sm" onClick={() => {
          if (index >= last) setPosition(0);
          setPlaying(!playing);
        }}>
          <Icon name={playing ? "stop" : "play"} className="size-3.5" /> {playing ? "Pause" : index >= last ? "Replay" : "Play"}
        </button>
        <ChipGroup<string>
          label="Speed"
          value={String(speed)}
          onChange={(value) => setSpeed(Number(value) as (typeof SPEEDS)[number])}
          chips={SPEEDS.map((value) => ({ value: String(value), label: `${value}×` }))}
        />
        <input
          type="range"
          min={0}
          max={last}
          step={1}
          value={index}
          aria-label="Position in the story"
          onChange={(event) => {
            setPlaying(false);
            setPosition(Number(event.target.value));
          }}
          className="range range-xs min-w-40 flex-1"
        />
        <span className="tabular text-sm text-base-content/70">
          {props.measured ? absoluteTime(frame.atMs, Date.now(), { seconds: true }) : `t = ${clock(frame.atMs - startMs)}`}
        </span>
        {props.actions?.(frame.atMs)}
      </div>

      <Panel id="stability-destinations">
        <div className="grid grid-cols-3 items-center gap-x-4 gap-y-2 md:grid-cols-[auto_1fr_1fr_1fr] md:gap-x-6" role="group" aria-label="Destinations now">
          <span className="col-span-3 text-lg font-semibold md:col-span-1">destinations</span>
          <Headline label="wanted" value={frame.wanted.toLocaleString()} />
          <div>
            <div className="kicker">not delivering</div>
            <div className={`tabular text-3xl font-semibold ${waiting > frame.wanted * 0.5 ? "text-error" : waiting > 0 ? "text-warning" : ""}`}>{waiting.toLocaleString()}</div>
            <Meter value={frame.wanted === 0 ? 0 : waiting / frame.wanted} tone={waiting > frame.wanted * 0.5 ? "bad" : waiting > 0 ? "warn" : "ok"} label="Share not delivering" className="mt-1" />
          </div>
          <Headline label="delivering" value={frame.delivering.toLocaleString()} tone={share < 0.5 ? "text-error" : share < 0.95 ? "text-warning" : ""} />
        </div>
      </Panel>

      <Panel
        id="stability-queues"
        title={props.title}
        actions={
          <span className="flex items-center gap-3 text-xs text-base-content/80">
            <Legend kind="fresh" label="new media" />
            <Legend kind="copy" label="re-sent copy" />
            <Legend kind="handshake" label="handshake" />
          </span>
        }
      >
        <ol className="flex flex-col gap-3" aria-label="Queues">
          {frame.queues.map((queue) => (
            <li key={queue.label} className="grid gap-2 md:grid-cols-[minmax(0,1fr)_14rem] md:items-center">
              <div className="min-w-0">
                <div className="mb-1 flex items-baseline justify-between gap-2 text-xs">
                  <span className="font-medium">{queue.label}'s queue</span>
                  <span className={`tabular ${queue.overflowing ? "font-semibold text-error" : "text-base-content/70"}`}>{queue.detail}</span>
                </div>
                <div className="flex h-8 gap-px overflow-hidden rounded-field border border-base-content/15 bg-base-100 p-0.5" role="img" aria-label={`${queue.label}: ${queue.cells.filter((cell) => cell !== "empty").length} of ${queue.cells.length} cells full`}>
                  {queue.cells.map((cell, cellIndex) => (
                    <span key={cellIndex} className={`h-full flex-1 rounded-[2px] transition-colors duration-300 ${CELL_CLASS[cell]}`} />
                  ))}
                </div>
                {queue.overflowing && (
                  <p className="mt-1 rounded-field bg-error/12 px-2 py-0.5 text-xs text-error">
                    ✕ backlog past the stall line: destinations behind it stall, resync and are torn down
                  </p>
                )}
              </div>
              <div className="flex flex-wrap items-center gap-1.5 rounded-box border border-base-content/10 bg-base-100 px-3 py-2 text-xs">
                {queue.utilization !== null && (
                  <span className="mr-auto tabular text-base-content/70">CPU {Math.round(queue.utilization * 100)}%</span>
                )}
                {(queue.states.live ?? 0) > 0 && <StatusBadge tone="ok">{queue.states.live} live</StatusBadge>}
                {(queue.states.stalled ?? 0) > 0 && <StatusBadge tone="bad">{queue.states.stalled} stalled</StatusBadge>}
                {(queue.states.connecting ?? 0) > 0 && <StatusBadge tone="info">{queue.states.connecting} connecting</StatusBadge>}
                {(queue.states.backoff ?? 0) > 0 && <StatusBadge tone="warn">{queue.states.backoff} backing off</StatusBadge>}
                {(queue.states.failed ?? 0) > 0 && <StatusBadge tone="bad">{queue.states.failed} gave up</StatusBadge>}
              </div>
            </li>
          ))}
        </ol>
      </Panel>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <MiniKpi label="reconnects" value={`${Math.round(frame.reconnects)}/s`} tone={frame.reconnects > 20 ? "text-error" : ""} />
        <MiniKpi label="work that is overhead" value={frame.wasted === null ? "--" : `${Math.round(frame.wasted * 100)}%`} tone={(frame.wasted ?? 0) > 0.3 ? "text-error" : ""} foot="handshakes and re-sent copies, not new media" />
        {frame.hottest === null && props.measured ? (
          <MiniKpi label="hottest shard CPU" value="--" foot="not in the flight record; see Inspect for shards now" />
        ) : (
          <div className="rounded-box border border-base-content/10 bg-base-200 px-4 py-3">
            <div className="kicker">hottest shard CPU</div>
            <div className="flex items-center gap-3">
              <span className="tabular text-2xl font-semibold">{frame.hottest === null ? "--" : `${Math.round(frame.hottest * 100)}%`}</span>
              <Meter value={frame.hottest} label="Hottest shard CPU" />
            </div>
          </div>
        )}
      </div>

      <Panel id="stability-chart" title="destinations over time">
        <TimeSeries
          // A measured span is drawn whole with a playhead, so the story's
          // shape is visible before it plays; a simulation unfolds.
          samples={props.measured ? frames : shown}
          fromMs={startMs}
          toMs={frames[last].atMs}
          label="Wanted, delivering and reconnects over time"
          height={160}
          bands={bands}
          marks={props.measured ? [...(props.marks ?? []), { atMs: frame.atMs, tone: "info" as const, kind: "playhead" }] : props.marks}
          aside={
            props.measured
              ? `${absoluteTime(frame.atMs, Date.now(), { seconds: true })} · ${speed}× speed`
              : `t = ${clock(frame.atMs - startMs)} · ${speed}× speed`
          }
          callout={
            stalled ? (
              <span className="rounded-full border-2 border-error bg-base-200/90 px-4 py-1.5 text-sm font-bold tracking-wide text-error">
                STALLED · 0 delivering
              </span>
            ) : undefined
          }
          series={[
            { id: "wanted", label: "wanted", pick: (sample) => sample.wanted, color: "text-base-content/60", dashed: true },
            { id: "delivering", label: "delivering", pick: (sample) => sample.delivering, color: "text-primary" },
            { id: "reconnects", label: "reconnects/s", pick: (sample) => sample.reconnects, color: "text-secondary" },
          ]}
        />
      </Panel>

      <section aria-live="polite" aria-label="Chapter" className="flex flex-col gap-2 px-1">
        <StatusBadge tone={chapterTone} className="self-start px-3 py-1 text-sm tracking-[0.12em] uppercase">
          {frame.chapter.label}
        </StatusBadge>
        <p className="text-lg leading-relaxed text-balance">{frame.chapter.caption}</p>
        <p className="text-xs text-base-content/60">{props.footer}</p>
      </section>
    </div>
  );
}

function Headline(props: { label: string; value: string; tone?: string }) {
  return (
    <div>
      <div className="kicker">{props.label}</div>
      <div className={`tabular text-3xl font-semibold ${props.tone ?? ""}`}>{props.value}</div>
    </div>
  );
}

function MiniKpi(props: { label: string; value: string; tone?: string; foot?: string }) {
  return (
    <div className="rounded-box border border-base-content/10 bg-base-200 px-4 py-3">
      <div className="kicker">{props.label}</div>
      <div className={`tabular text-2xl font-semibold ${props.tone ?? ""}`}>{props.value}</div>
      {props.foot && <div className="text-xs text-base-content/70">{props.foot}</div>}
    </div>
  );
}

function Legend(props: { kind: CellKind; label: string }) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <span aria-hidden="true" className={`size-2.5 rounded-[2px] ${CELL_CLASS[props.kind]}`} />
      {props.label}
    </span>
  );
}
