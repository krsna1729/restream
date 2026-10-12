import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { buildResourceRows, ringFromGraph, shardsFrom } =
  await loadCompiledFrontendModule("control-room/inspect-model.js");
const { appendSample, sparkline, takeSample } = await loadCompiledFrontendModule(
  "control-room/monitor-model.js",
);

function resource(id, overrides = {}) {
  return { id, kind: "egress", label: id, ...overrides };
}

function memory(bytes, confidence = "derived") {
  return { attributedBytes: bytes, confidence, source: "test" };
}

function shard(protocol, index, overrides = {}) {
  return resource(`shard-${protocol}-${index}`, {
    kind: "egress_shard",
    protocol,
    shardIndex: index,
    state: "healthy",
    metrics: { progressAgeMs: 5, commandDepth: 1, commandCapacity: 1024, mediaTicks: 9 },
    ...overrides,
  });
}

function snapshot(nodes) {
  return { generatedAt: "", scope: { kind: "pipeline" }, summary: {}, nodes };
}

test("resource rows: a hotspot first, then by memory held, unknown memory last", () => {
  const rows = buildResourceRows(
    snapshot([
      resource("small", { memory: memory(10) }),
      resource("unknown"),
      resource("big", { memory: memory(1_000, "measured") }),
      resource("hot", { memory: memory(1), hotspots: ["backpressure"] }),
    ]),
  );
  assert.deepEqual(
    rows.map((row) => row.node.id),
    ["hot", "big", "small", "unknown"],
  );
  assert.equal(rows[1].confidence, "measured");
  assert.equal(rows[3].memoryBytes, null);
});

test("the ring comes from the graph; none, or an unsized one, is no ring", () => {
  const ringNode = (details) => ({
    id: "rb",
    type: "ring_buffer",
    label: "Source Buffer",
    active: true,
    details,
  });
  const graph = (nodes) => ({ pipelineId: "p", nodes, edges: [] });
  assert.deepEqual(
    ringFromGraph(
      graph([
        ringNode({
          fill: 37,
          capacity: 1024,
          payloadStats: { payloadBytes: 2048 },
        }),
      ]),
    ),
    { fill: 37, capacity: 1024, payloadBytes: 2048 },
  );
  assert.equal(ringFromGraph(graph([])), null);
  assert.equal(ringFromGraph(graph([ringNode(null)])), null);
  assert.equal(ringFromGraph(graph([ringNode({ fill: 1, capacity: 0 })])), null);
});

function output(id, pipelineId, overrides = {}) {
  return {
    id,
    pipelineId,
    name: id,
    protocol: "rtmp",
    shard: null,
    desired: "running",
    actual: "sending",
    convergence: "converged",
    flow: "flowing",
    backpressure: null,
    retry: null,
    evidence: null,
    diagnosis: null,
    ...overrides,
  };
}

test("shards: only the ones this pipeline's outputs are placed on, by protocol then index", () => {
  const map = snapshot([
    shard("srt", 1),
    // Not a shard, though it names one.
    resource("output", { protocol: "rtmp", shardIndex: 0 }),
    shard("rtmp", 0, { state: "stalled", metrics: { progressAgeMs: 900 } }),
    shard("srt", 0),
    // Another pipeline's shard.
    shard("rtmp", 3),
  ]);
  const shards = shardsFrom(map, [
    output("a", "p", { protocol: "rtmp", shard: 0 }),
    output("b", "p", { protocol: "srt", shard: 1 }),
    output("c", "p", { protocol: "srt", shard: 0 }),
    // Not placed (not running): no shard.
    output("d", "p", { protocol: "rtmp", shard: null }),
  ]);
  assert.deepEqual(
    shards.map((s) => `${s.protocol}:${s.index}:${s.state}`),
    ["rtmp:0:stalled", "srt:0:healthy", "srt:1:healthy"],
  );
  assert.equal(shards[0].progressAgeMs, 900);
  assert.equal(shards[0].commandDepth, null);
  // Same index, other protocol: not this pipeline's.
  assert.deepEqual(shardsFrom(map, [output("x", "p", { protocol: "hls", shard: 0 })]), []);
});

test("a sample covers one pipeline: its outputs, rates, lag, ring and shards", () => {
  const sample = takeSample({
    atMs: 1_000,
    pipelineId: "p",
    outputs: [
      output("a", "p", { shard: 1 }),
      output("b", "p", { convergence: "starting", flow: "idle" }),
      output("off", "p", { desired: "stopped", flow: "idle" }),
      output("other", "q"),
    ],
    rates: {
      ingestBps: new Map([["p", 8_000_000], ["q", 1]]),
      egressBps: new Map([["a", 3_000_000], ["other", 9_000_000]]),
    },
    graph: {
      pipelineId: "p",
      nodes: [
        { id: "rb", type: "ring_buffer", label: "", active: true, details: { fill: 256, capacity: 1024 } },
        { id: "e-a", type: "egress", label: "", active: true, outputId: "a", details: { feedLagUnits: 4 } },
        { id: "e-b", type: "egress", label: "", active: false, outputId: "b", details: null },
      ],
      edges: [],
    },
    // Shard 0 is slower but carries another pipeline's outputs only.
    resources: snapshot([
      shard("rtmp", 0, { metrics: { progressAgeMs: 900 } }),
      shard("rtmp", 1, { metrics: { progressAgeMs: 40 } }),
    ]),
  });
  assert.deepEqual(sample, {
    atMs: 1_000,
    ingestBps: 8_000_000,
    egressBps: 3_000_000,
    // One of two wanted outputs; the stopped one is not wanted.
    deliveringPercent: 50,
    maxFeedLag: 4,
    ringFillPercent: 25,
    maxShardProgressMs: 40,
  });

  // No source: a gap, never a zero.
  const empty = takeSample({
    atMs: 2_000,
    pipelineId: "p",
    outputs: [output("off", "p", { desired: "stopped" })],
    rates: null,
    graph: null,
    resources: null,
  });
  assert.deepEqual(
    [empty.ingestBps, empty.egressBps, empty.deliveringPercent, empty.maxFeedLag],
    [null, null, null, null],
  );
});

test("history keeps the newest samples up to its cap", () => {
  let history = [];
  for (let i = 0; i < 5; i += 1) {
    history = appendSample(history, { atMs: i }, 3);
  }
  assert.deepEqual(
    history.map((sample) => sample.atMs),
    [2, 3, 4],
  );
});

test("a sparkline spans the window, peaks at the top and breaks at a gap", () => {
  const history = [
    { atMs: 0, ingestBps: 0 },
    { atMs: 50, ingestBps: 10 },
    { atMs: 75, ingestBps: null },
    { atMs: 100, ingestBps: 5 },
  ];
  assert.deepEqual(sparkline(history, "ingestBps", 0, 100, 200, 40), [
    "0.0,40.0 100.0,0.0",
    "200.0,20.0",
  ]);
  // All zero: a flat line on the floor, not a division by zero.
  assert.deepEqual(
    sparkline([{ atMs: 0, ingestBps: 0 }, { atMs: 10, ingestBps: 0 }], "ingestBps", 0, 10, 100, 40),
    ["0.0,40.0 100.0,40.0"],
  );
});
