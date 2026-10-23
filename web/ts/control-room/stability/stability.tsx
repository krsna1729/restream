// The Stability Lab: watch an overload become a metastable failure (or
// not) in restream's own topology, as a narrated story. Simulate a built-in
// scenario under the engine's real reconnect policy or a naive one, or
// replay a pipeline's measured flight record in the same view.

import { useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import { getCapacityScenario } from "../../core/api.js";
import { ChipGroup } from "../../ui/controls.js";
import { Icon } from "../../ui/icons.js";
import { Meter } from "../../ui/kpi.js";
import { PageHeader } from "../../ui/page-header.js";
import { EmptyState, Panel } from "../../ui/panel.js";
import { TimeSeries } from "../../ui/time-series.js";
import type { Band, Mark } from "../../ui/time-series.js";
import { StatusBadge } from "../../ui/tone.js";
import { useFlightRecorder, usePolled } from "../polled.js";
import type { LiveSnapshot } from "../store.js";
import { ENGINE_POLICY, FRAME_MS, SCENARIOS, simulate } from "./sim-engine.js";
import type { RetryPolicy, Scenario, ScenarioEvent } from "./sim-engine.js";
import { storyFromSim, storyFromTrace } from "./story-model.js";
import type { CellKind, StoryFrame } from "./story-model.js";

type Mode = "simulate" | "replay";
type PolicyChoice = "engine" | "fixed" | "immediate";

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

export function StabilityLab({ live }: { live: LiveSnapshot }) {
  const [mode, setMode] = useState<Mode>("simulate");
  const [pipelineId, setPipelineId] = useState<string | null>(null);
  const firstPipeline = live.state?.pipelines.find((pipeline) => pipeline.ingest)?.id ?? live.state?.pipelines[0]?.id ?? null;
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Stability Lab"
        title="How restream rides out an overload"
        subtitle="A short trigger can leave a system stuck long after it ends. Watch restream's egress, shard by shard, under its real reconnect policy and under naive ones, or replay what a pipeline actually did."
        actions={
          <ChipGroup<Mode>
            label="Mode"
            value={mode}
            onChange={setMode}
            chips={[
              { value: "simulate", label: "Simulate a scenario" },
              { value: "replay", label: "Replay a pipeline" },
            ]}
          />
        }
      />
      {mode === "simulate" ? (
        <Simulator />
      ) : (
        <Replay
          live={live}
          pipelineId={pipelineId ?? firstPipeline}
          onPipeline={setPipelineId}
        />
      )}
    </div>
  );
}

function Simulator() {
  const calibration = usePolled(["stability-retry-policy"], () => getCapacityScenario({ addOutputs: {}, addStages: 0 }), 60_000);
  const enginePolicy: RetryPolicy = calibration.value?.retryPolicy
    ? { kind: "engine", baseMs: calibration.value.retryPolicy.baseMs, maxMs: calibration.value.retryPolicy.maxMs, maxRetries: calibration.value.retryPolicy.maxRetries }
    : ENGINE_POLICY;
  const [scenarioId, setScenarioId] = useState(SCENARIOS[0].id);
  const [policyChoice, setPolicyChoice] = useState<PolicyChoice>("engine");
  const [paced, setPaced] = useState(false);
  const [actions, setActions] = useState<ScenarioEvent[]>([]);
  const base = SCENARIOS.find((scenario) => scenario.id === scenarioId) ?? SCENARIOS[0];
  const scenario: Scenario = useMemo(
    () => ({
      ...base,
      events: [
        ...base.events.map((event) => (event.kind === "spike" ? { ...event, spreadMs: paced ? 10_000 : 1_000 } : event)),
        ...actions,
      ],
    }),
    [base, paced, actions],
  );
  const policy: RetryPolicy =
    policyChoice === "engine"
      ? enginePolicy
      : policyChoice === "fixed"
        ? { kind: "fixed", baseMs: 1_000, maxMs: 1_000, maxRetries: Number.MAX_SAFE_INTEGER }
        : { kind: "immediate", baseMs: 0, maxMs: 0, maxRetries: Number.MAX_SAFE_INTEGER };
  const story = useMemo(() => storyFromSim(simulate(scenario, policy, 7), scenario), [scenario, policy.kind, policy.baseMs, policy.maxMs, policy.maxRetries]);
  const footer = `restream egress model · ${scenario.shards} shards · ${base.destinations} destinations · ${policyChoice === "engine" ? `engine backoff ${policy.baseMs / 1000} s → ${policy.maxMs / 1000} s, equal jitter` : policyChoice === "fixed" ? "retry every 1 s" : "retry at once"} · modelled, not measured`;
  return (
    <>
      <div className="grid gap-3 md:grid-cols-3">
        {SCENARIOS.map((candidate) => (
          <button
            key={candidate.id}
            type="button"
            aria-pressed={candidate.id === scenarioId}
            onClick={() => {
              setScenarioId(candidate.id);
              setActions([]);
            }}
            className={`rounded-box border p-3 text-left transition-colors ${candidate.id === scenarioId ? "border-primary bg-primary/10" : "border-base-content/10 bg-base-200 hover:border-base-content/30"}`}
          >
            <div className="font-semibold">{candidate.title}</div>
            <p className="mt-1 line-clamp-3 text-xs text-base-content/70">{candidate.summary}</p>
          </button>
        ))}
      </div>
      <div className="flex flex-wrap items-center gap-3">
        <ChipGroup<PolicyChoice>
          label="Reconnect policy"
          value={policyChoice}
          onChange={setPolicyChoice}
          chips={[
            { value: "engine", label: "Engine backoff" },
            { value: "fixed", label: "Retry every 1 s" },
            { value: "immediate", label: "Retry at once" },
          ]}
        />
        {base.events.some((event) => event.kind === "spike") && (
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" className="toggle toggle-sm" checked={paced} onChange={(event) => setPaced(event.target.checked)} />
            Pace the starts over 10 s
          </label>
        )}
      </div>
      <Player
        key={`${scenarioId}-${policyChoice}-${paced}`}
        frames={story}
        footer={footer}
        title={`egress · ${scenario.shards} shards`}
        actions={(atMs) => (
          <button
            type="button"
            className="btn btn-sm"
            title="Restart the most backlogged shard: its queue is dropped and its destinations reconnect."
            onClick={() => {
              const frame = story.find((candidate) => candidate.atMs >= atMs) ?? story[story.length - 1];
              const worst = frame.queues.reduce((best, queue, index) => (queue.fill > (frame.queues[best]?.fill ?? -1) ? index : best), 0);
              setActions((current) => [...current, { atMs, kind: "restartShard", shard: worst }]);
            }}
          >
            <Icon name="refresh" className="size-3.5" /> Restart worst shard here
          </button>
        )}
        marks={actions.map((event) => ({ atMs: event.atMs, label: event.kind === "restartShard" ? `restart shard ${event.shard}` : undefined, tone: "ok" as const }))}
      />
    </>
  );
}

function Replay(props: { live: LiveSnapshot; pipelineId: string | null; onPipeline: (id: string) => void }) {
  if (!props.pipelineId) return <EmptyState title="No pipeline to replay" detail="Create a pipeline; the flight recorder keeps its last 10 minutes." />;
  return (
    <>
      <div className="flex flex-wrap items-center gap-3">
        <label className="flex items-center gap-2 text-sm">
          Pipeline
          <select className="select select-sm w-64" value={props.pipelineId} onChange={(event) => props.onPipeline(event.target.value)}>
            {(props.live.state?.pipelines ?? []).map((pipeline) => (
              <option key={pipeline.id} value={pipeline.id}>
                {pipeline.name}
              </option>
            ))}
          </select>
        </label>
        <span className="text-sm text-base-content/70">The engine's flight recorder: the last 10 minutes, sampled every 2 s.</span>
      </div>
      <ReplayPlayer key={props.pipelineId} pipelineId={props.pipelineId} />
    </>
  );
}

function ReplayPlayer(props: { pipelineId: string }) {
  const trace = useFlightRecorder(props.pipelineId);
  const frames = useMemo(() => (trace.value ? storyFromTrace(trace.value) : []), [trace.value]);
  if (trace.value === null) return <EmptyState title="Waiting for the flight recorder…" />;
  if (frames.length < 2) return <EmptyState title="Collecting samples…" detail="Two samples are needed to replay." />;
  return <Player frames={frames} footer="restream flight recorder · measured samples every 2 s, modelled in between" title="pipeline" follow />;
}

function Player(props: {
  frames: readonly StoryFrame[];
  footer: string;
  title: string;
  /** Live replay: stay at the newest frame unless the operator scrubs. */
  follow?: boolean;
  actions?: (atMs: number) => ReactNode;
  marks?: readonly Mark[];
}) {
  const { frames } = props;
  const [position, setPosition] = useState(props.follow ? frames.length - 1 : 0);
  const [playing, setPlaying] = useState(!props.follow);
  const [speed, setSpeed] = useState<(typeof SPEEDS)[number]>(4);
  const last = frames.length - 1;
  const step = props.follow ? (frames[1]?.atMs ?? 2000) - (frames[0]?.atMs ?? 0) : FRAME_MS;
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
        <span className="tabular text-sm text-base-content/70">t = {clock(frame.atMs - startMs)}</span>
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
        <div className="rounded-box border border-base-content/10 bg-base-200 px-4 py-3">
          <div className="kicker">hottest shard CPU</div>
          <div className="flex items-center gap-3">
            <span className="tabular text-2xl font-semibold">{frame.hottest === null ? "--" : `${Math.round(frame.hottest * 100)}%`}</span>
            <Meter value={frame.hottest} label="Hottest shard CPU" />
          </div>
        </div>
      </div>

      <Panel id="stability-chart" title="destinations over time">
        <TimeSeries
          samples={shown}
          fromMs={startMs}
          toMs={frames[last].atMs}
          label="Wanted, delivering and reconnects over time"
          height={160}
          bands={bands}
          marks={props.marks}
          aside={`t = ${clock(frame.atMs - startMs)} · ${speed}× speed`}
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
