import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { buildResourceRows, nodeCards, ringFromGraph, shardsFrom } =
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
  const feedId = overrides.feedId ?? `${protocol}:p:source`;
  return resource(`shard-${feedId}-${index}`, {
    kind: "egress_shard",
    protocol,
    feedId,
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

test("shards: by protocol, then feed (each numbers from 0), then index", () => {
  const shards = shardsFrom(
    snapshot([
      shard("srt", 1),
      // Not a shard, though it names one.
      resource("output", { protocol: "rtmp", shardIndex: 0 }),
      shard("rtmp", 0, { state: "stalled", metrics: { progressAgeMs: 900 } }),
      shard("rtmp", 0, { feedId: "rtmp:p:720p" }),
      shard("srt", 0),
    ]),
  );
  assert.deepEqual(
    shards.map((s) => `${s.feed}:${s.index}:${s.state}`),
    ["rtmp:p:720p:0:healthy", "rtmp:p:source:0:stalled", "srt:p:source:0:healthy", "srt:p:source:1:healthy"],
  );
  assert.equal(shards[1].progressAgeMs, 900);
  assert.equal(shards[1].commandDepth, null);
});

test("a sample covers one pipeline: its outputs, rates, lag, ring and shards", () => {
  const sample = takeSample({
    atMs: 1_000,
    pipelineId: "p",
    outputs: [
      output("a", "p", { shard: 0 }),
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
    resources: snapshot([
      shard("rtmp", 0, { metrics: { progressAgeMs: 40 } }),
      shard("rtmp", 0, { feedId: "rtmp:p:720p", metrics: { progressAgeMs: 7 } }),
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

  // A truncated map may have dropped the slowest shard: a gap.
  const truncated = takeSample({
    atMs: 1_500,
    pipelineId: "p",
    outputs: [],
    rates: null,
    graph: null,
    resources: { ...snapshot([shard("rtmp", 0)]), limits: { truncatedNodeCount: 3 } },
  });
  assert.equal(truncated.maxShardProgressMs, null);

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

test("history keeps the window by time, however late the timer fires", () => {
  let history = [];
  // A hidden tab: samples 10 s apart, then 70 s and 10 s.
  for (const atMs of [0, 10_000, 20_000, 90_000, 100_000]) {
    history = appendSample(history, { atMs }, 60_000);
  }
  assert.deepEqual(
    history.map((sample) => sample.atMs),
    [90_000, 100_000],
  );
});

test("a sparkline spans the window, peaks at the top and breaks at a gap", () => {
  const history = [
    { atMs: 0, ingestBps: 0 },
    { atMs: 50, ingestBps: 10 },
    { atMs: 75, ingestBps: null },
    { atMs: 100, ingestBps: 5 },
  ];
  // The lone last value is a zero-length segment: a dot, not nothing.
  assert.deepEqual(sparkline(history, "ingestBps", 0, 100, 200, 40), [
    "0.0,40.0 100.0,0.0",
    "200.0,20.0 200.0,20.0",
  ]);
  // All zero: a flat line on the floor, not a division by zero.
  assert.deepEqual(
    sparkline([{ atMs: 0, ingestBps: 0 }, { atMs: 10, ingestBps: 0 }], "ingestBps", 0, 10, 100, 40),
    ["0.0,40.0 100.0,40.0"],
  );
});

test("node cards count the stage's graph consumers and omit numbers the node lacks", () => {
  const graph = {
    nodes: [
      { id: "ring", type: "ring_buffer", label: "ring", active: true },
      { id: "tx", type: "stage", label: "tx", active: true, stageKey: "stage-1" },
      { id: "e1", type: "egress", label: "e1", active: true },
      { id: "e2", type: "egress", label: "e2", active: true },
    ],
    edges: [
      { from: "ring", to: "tx" },
      { from: "ring", to: "e1" },
      { from: "tx", to: "e2" },
      { from: "tx", to: "e1" },
    ],
  };
  const stage = resource("stage-1", {
    kind: "stage",
    cpuPercent: 12.345,
    backendPid: 4242,
    metrics: { packetsPerSec: 1234.4, avgUsPerPacket: 3.21 },
  });
  assert.deepEqual(nodeCards(stage, graph), [
    { label: "Consumers", value: "2", foot: "graph nodes reading it" },
    { label: "CPU", value: "12.3%", foot: "measured child PID 4242" },
    { label: "Packet rate", value: "1,234/s", foot: "input packets, since it started" },
    { label: "Average work", value: "3.2 µs", foot: "per input packet" },
  ]);

  // The source ring's consumers are the ring buffer's edges.
  const ring = resource("source", { kind: "source_ring" });
  assert.deepEqual(nodeCards(ring, graph), [
    { label: "Consumers", value: "2", foot: "graph nodes reading it" },
  ]);

  // No graph (the global map), no CPU sample, no work recorded: no cards,
  // not zeros.
  const idle = resource("stage-1", {
    kind: "stage",
    cpuPercent: null,
    metrics: { packetsPerSec: Number.NaN, avgUsPerPacket: 0 },
  });
  assert.deepEqual(nodeCards(idle, null), []);
});
