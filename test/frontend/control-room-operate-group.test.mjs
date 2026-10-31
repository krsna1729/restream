import assert from "node:assert/strict";
import test from "node:test";

import { buildMediaGraph } from "../../web/ts/control-room/operate-model.ts";
import { inspectGroup } from "../../web/ts/control-room/operate-group-model.ts";

function output(id, overrides = {}) {
  return {
    id,
    pipelineId: "p",
    name: id,
    protocol: "srt",
    shard: 0,
    stage: "p:source",
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

function node(id, type, label = id) {
  return { id, type, label, active: true, details: null, metrics: null };
}

function egressNode(outputId) {
  return { ...node(`egress-${outputId}`, "egress"), outputId };
}

function running(outputId, overrides = {}) {
  return {
    outputId,
    fabricFeed: "srt:p:source",
    shardId: 0,
    feedLagUnits: 1,
    resyncCount: 0,
    quality: { deliveryRatio: 0.99 },
    ...overrides,
  };
}

function shard(shardIndex, overrides = {}) {
  return {
    protocol: "srt",
    feedId: "srt:p:source",
    shardIndex,
    state: "healthy",
    commandDepth: 0,
    commandCapacity: 1024,
    progressAgeMs: 2,
    ...overrides,
  };
}

const LEAF_STALLED = {
  flow: "stalled",
  diagnosis: {
    layer: "destination",
    cause: { code: "backpressured", stalled: true },
    ruledOut: ["ingest", "feed", "shard"],
  },
};

test("a group names the stages that feed it and the outputs it holds", () => {
  const outputs = [output("a"), output("b"), output("r", { protocol: "rtmp" })];
  const columns = buildMediaGraph(
    {
      nodes: [
        node("ring", "ring_buffer", "Source ring"),
        egressNode("a"),
        egressNode("b"),
        egressNode("r"),
      ],
      edges: ["a", "b", "r"].map((id) => ({ from: "ring", to: `egress-${id}` })),
    },
    outputs,
  );
  const groups = columns.flatMap((column) => column.groups);
  assert.deepEqual(
    groups.map(({ protocol, upstream, outputIds }) => ({ protocol, upstream, outputIds })),
    [
      { protocol: "srt", upstream: "Source ring", outputIds: ["a", "b"] },
      { protocol: "rtmp", upstream: "Source ring", outputIds: ["r"] },
    ],
  );
});

test("the inspection reads only the group's outputs: worst delivery, lag, resyncs, shards, faults", () => {
  const outputs = [
    output("a"),
    output("b", LEAF_STALLED),
    output("c", { shard: 1 }),
    // Another group's output must not leak into this one.
    output("other", { ...LEAF_STALLED, protocol: "rtmp" }),
  ];
  const group = {
    key: "ring|srt",
    protocol: "srt",
    upstream: "Source ring",
    outputIds: ["a", "b", "c"],
    outputs: 3,
    delivering: 2,
    attention: 1,
  };
  const egresses = [
    running("a", { resyncCount: 1 }),
    running("b", { feedLagUnits: 147, quality: { deliveryRatio: 0.834 }, resyncCount: 1 }),
    running("c", { shardId: 1 }),
    running("other", { feedLagUnits: 999, quality: { deliveryRatio: 0.1 }, resyncCount: 9 }),
  ];
  const inspection = inspectGroup(group, outputs, egresses, [
    shard(0, { commandDepth: 2 }),
    shard(7),
  ]);
  assert.equal(inspection.delivering, 2);
  assert.equal(inspection.outputs, 3);
  assert.equal(inspection.minDeliveryRatio, 0.834);
  assert.equal(inspection.worstFeedLag, 147);
  assert.equal(inspection.resyncs, 2);
  // Shard 1 carries output c but is not in the health report yet.
  assert.deepEqual(
    inspection.shards.map((s) => [s.shardIndex, s.status?.commandDepth ?? null]),
    [
      [0, 2],
      [1, null],
    ],
  );
  assert.deepEqual(inspection.degraded, [
    {
      id: "b",
      name: "b",
      layer: "destination",
      cause: "Stuck: the server is not accepting data",
      ruledOut: ["ingest", "feed", "shard"],
    },
  ]);
});

test("without telemetry the inspection reports no figures rather than zeros", () => {
  const group = {
    key: "ring|srt",
    protocol: "srt",
    upstream: "Source ring",
    outputIds: ["a"],
    outputs: 1,
    delivering: 1,
    attention: 0,
  };
  const inspection = inspectGroup(group, [output("a")], null, null);
  assert.equal(inspection.minDeliveryRatio, null);
  assert.equal(inspection.worstFeedLag, null);
  assert.deepEqual(inspection.shards, []);
});
