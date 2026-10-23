// The Stability Lab: watch an overload become a metastable failure (or
// not) in restream's own topology, as a narrated story. Simulate a built-in
// scenario under the engine's real reconnect policy or a naive one, or
// replay a pipeline's measured flight record in the same view.

import { useMemo, useState } from "react";

import { getCapacityScenario } from "../../core/api.js";
import { ChipGroup } from "../../ui/controls.js";
import { Icon } from "../../ui/icons.js";
import { PageHeader } from "../../ui/page-header.js";
import { usePolled } from "../polled.js";
import { useGo, usePlace } from "../router.js";
import type { LiveSnapshot } from "../store.js";
import { Player } from "./player.js";
import { Replay } from "./replay.js";
import { lastSpan } from "./replay-window.js";
import { ENGINE_POLICY, SCENARIOS, simulate } from "./sim-engine.js";
import type { RetryPolicy, Scenario, ScenarioEvent } from "./sim-engine.js";
import { storyFromSim } from "./story-model.js";

type Mode = "simulate" | "replay";
type PolicyChoice = "engine" | "fixed" | "immediate";

export function StabilityLab({ live }: { live: LiveSnapshot }) {
  const place = usePlace();
  const go = useGo();
  const [mode, setMode] = useState<Mode>(place.replay ? "replay" : "simulate");
  const firstPipeline = live.state?.pipelines.find((pipeline) => pipeline.ingest)?.id ?? live.state?.pipelines[0]?.id ?? null;
  // The replay's pipeline and span are the URL's, so a link reopens them.
  const window = place.replay ?? lastSpan(10 * 60_000, Date.now());
  return (
    <div className="flex flex-col gap-4">
      <PageHeader
        eyebrow="Stability Lab"
        title="How restream rides out an overload"
        subtitle="A short trigger can leave a system stuck long after it ends. Watch restream's egress, shard by shard, under its real reconnect policy and under naive ones, or replay what a pipeline actually did over any span its history keeps."
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
          pipelineId={place.pipelineId ?? firstPipeline}
          window={window}
          onChange={(pipelineId, next) => go({ workspace: "stability", pipelineId, replay: next })}
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
