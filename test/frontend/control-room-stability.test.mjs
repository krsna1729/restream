import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { ENGINE_POLICY, SCENARIOS, backoffMs, simulate } = await loadCompiledFrontendModule(
  "control-room/stability/sim-engine.js",
);

const FIXED_1S = { kind: "fixed", baseMs: 1000, maxMs: 1000, maxRetries: 1e9 };
const IMMEDIATE = { kind: "immediate", baseMs: 0, maxMs: 0, maxRetries: 1e9 };

const scenario = (id) => SCENARIOS.find((candidate) => candidate.id === id);
function deliveringAt(frames, atMs) {
  const frame = frames.find((candidate) => candidate.atMs === atMs);
  return frame.delivering / frame.wanted;
}

test("the engine's backoff is exponential, capped, and keeps half of each wait fixed", () => {
  assert.equal(backoffMs(ENGINE_POLICY, 0, 0), 2_500);
  assert.equal(backoffMs(ENGINE_POLICY, 0, 0.999_999) < 5_000, true);
  assert.equal(backoffMs(ENGINE_POLICY, 3, 0), 20_000);
  // Capped at maxMs, so the fixed half is half the cap.
  assert.equal(backoffMs(ENGINE_POLICY, 12, 0), 150_000);
  assert.equal(backoffMs(FIXED_1S, 9, 0.5), 1_000);
  assert.equal(backoffMs(IMMEDIATE, 9, 0.5), 0);
});

test("the same scenario and seed give the same trace", () => {
  const a = simulate(scenario("platform-outage"), ENGINE_POLICY, 3);
  const b = simulate(scenario("platform-outage"), ENGINE_POLICY, 3);
  assert.deepEqual(a, b);
  assert.notDeepEqual(a, simulate(scenario("platform-outage"), ENGINE_POLICY, 4));
});

// The model's point: a 20 s platform outage is survivable with the engine's
// backoff, and a permanent failure without it. The trigger is gone at 40 s;
// with a naive retry the handshakes it caused keep every shard saturated.
test("a platform blip: the engine's backoff recovers, naive retries stay stuck", () => {
  const blip = scenario("platform-outage");
  const engine = simulate(blip, ENGINE_POLICY, 7);
  assert.equal(deliveringAt(engine, 10_000), 1, "healthy before the outage");
  assert.ok(deliveringAt(engine, 30_000) >= 0.55, "during it only the platform's share is lost");
  assert.ok(deliveringAt(engine, 70_000) >= 0.95, "recovered half a minute after it ends");
  for (const policy of [FIXED_1S, IMMEDIATE]) {
    const frames = simulate(blip, policy, 7);
    assert.ok(deliveringAt(frames, blip.durationMs) <= 0.05, `${policy.kind}: still stuck at the end`);
  }
});

test("200 starts at once tip the plant over; the same starts paced over 10 s do not", () => {
  const spike = scenario("spike");
  assert.ok(deliveringAt(simulate(spike, ENGINE_POLICY, 7), spike.durationMs) <= 0.05);
  const paced = { ...spike, events: [{ ...spike.events[0], spreadMs: 10_000 }] };
  assert.equal(deliveringAt(simulate(paced, ENGINE_POLICY, 7), 60_000), 1);
});

test("frames account for every destination and the wasted share is a fraction", () => {
  for (const frame of simulate(scenario("noisy-neighbour"), ENGINE_POLICY, 1)) {
    const counted = frame.shards.reduce(
      (sum, shard) => sum + Object.values(shard.states).reduce((a, b) => a + b, 0),
      0,
    );
    assert.equal(counted, frame.wanted);
    assert.ok(frame.wastedShare >= 0 && frame.wastedShare <= 1);
  }
});

const { CELLS, queueCells, storyFromSim, storyFromTrace } = await loadCompiledFrontendModule(
  "control-room/stability/story-model.js",
);

test("queue cells keep the strip's scale and colour by what the queue holds", () => {
  assert.deepEqual(queueCells({ fresh: 0, copy: 0, handshake: 0 }, 100), Array(CELLS).fill("empty"));
  const half = queueCells({ fresh: 25, copy: 0, handshake: 25 }, 100);
  assert.equal(half.filter((cell) => cell !== "empty").length, CELLS / 2);
  assert.equal(half.filter((cell) => cell === "handshake").length, CELLS / 4);
  // Over the scale: full, never longer.
  assert.equal(queueCells({ fresh: 500, copy: 500, handshake: 0 }, 100).length, CELLS);
  assert.ok(!queueCells({ fresh: 500, copy: 500, handshake: 0 }, 100).includes("empty"));
});

test("the story narrates healthy, the trigger, being stuck, and recovering", () => {
  const blip = scenario("platform-outage");
  const chapters = (policy) =>
    new Set(storyFromSim(simulate(blip, policy, 7), blip).map((frame) => frame.chapter.id));
  const engine = chapters(ENGINE_POLICY);
  assert.ok(engine.has("healthy") && engine.has("trigger") && engine.has("recovered"));
  assert.ok(!engine.has("stuck"));
  const naive = chapters(IMMEDIATE);
  assert.ok(naive.has("stuck"));
  assert.ok(!naive.has("recovered"));
});

test("a measured trace reads as a story without inventing numbers it lacks", () => {
  const outputs = { wanted: 4, delivering: 2, degraded: 2, retrying: 2, stalled: 0, retryAttempts: 5, offeredBps: 1000, deliveredBps: 500, resyncs: 0, worstFeedLag: 0 };
  const [frame] = storyFromTrace({
    schema: "restream.trace.v1",
    pipelineId: "p",
    intervalMs: 2000,
    completeFromMs: 0,
    samples: [{ atMs: 1, ingest: { inputId: "i", protocol: "srt" }, ring: { fill: 512, capacity: 1024, worstLagSlots: 3, overflows: 0 }, outputs }],
  });
  assert.equal(frame.chapter.label, "Retrying");
  assert.equal(frame.queues[0].fill, 0.5);
  assert.equal(frame.wasted, 0.5);
  assert.equal(frame.hottest, null, "the recorder does not measure shard CPU per pipeline");
});
