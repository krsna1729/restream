import assert from "node:assert/strict";
import test from "node:test";

import { installFakeDom } from "../support/helpers/fake-dom.mjs";

async function load() {
  installFakeDom();
  return import("../../web/ts/control-room/state-views.ts");
}

function input(id, position, connected) {
  return {
    id,
    pipelineId: "pipe",
    position,
    label: id,
    role: position === 0 ? "primary" : "backup",
    selected: position === 0,
    runtime: { connected, forwardingState: connected ? "active" : null, protocol: null },
  };
}

// The deck and Monitor's transport read inputs from the state in the order
// the REST list had them, with the sampled RTT on the input it belongs to; a
// row sampled before a disconnect must not show on the disconnected input.
test("inputs come in list order with their own session's counters", async () => {
  const { pipelineInputs } = await load();
  const state = {
    inputs: [input("b-backup", 1, false), input("a-main", 0, true), { ...input("x", 0, true), pipelineId: "other" }],
    graphs: [],
  };
  const telemetry = {
    inputs: [
      { inputId: "a-main", pipelineId: "pipe", uptimeSecs: 5, bytesReceived: 10, quality: { msRtt: 12 } },
      { inputId: "b-backup", pipelineId: "pipe", uptimeSecs: 1, bytesReceived: 1, quality: { msRtt: 99 } },
    ],
  };
  const inputs = pipelineInputs(state, telemetry, "pipe");
  assert.deepEqual(
    inputs.map((entry) => [entry.id, entry.runtime.quality?.msRtt ?? null]),
    [
      ["a-main", 12],
      ["b-backup", null],
    ],
  );
  assert.equal(pipelineInputs(null, telemetry, "pipe"), null, "no state yet: loading");
});

// The graph's counters come from the telemetry rows of the nodes they
// describe: Operate's packet rates, Monitor's ring fill and feed lag.
test("the graph carries each node's sampled counters", async () => {
  const { pipelineGraph } = await load();
  const node = (id, type, extra = {}) => ({
    id,
    type,
    label: id,
    active: true,
    stageKey: null,
    outputId: null,
    metricsStage: null,
    ...extra,
  });
  const state = {
    inputs: [],
    graphs: [
      {
        pipelineId: "pipe",
        nodes: [
          node("pipe_ingest", "ingest"),
          node("pipe_source_rb", "ring_buffer"),
          node("pipe_video_720p", "transcoder", { stageKey: "video:720p", metricsStage: "pipe:video:720p" }),
          node("pipe_output_o", "egress", { outputId: "o" }),
        ],
        edges: [{ from: "pipe_source_rb", to: "pipe_output_o", label: "" }],
      },
    ],
  };
  const telemetry = {
    ingests: [{ pipelineId: "pipe", metrics: { packetsPerSec: 50 } }],
    stages: [{ stageKey: "pipe:video:720p", metrics: { packetsPerSec: 30 } }],
    egresses: [{ outputId: "o", feedLagUnits: 7 }],
    memoryAccounting: {
      sourceRings: [{ pipelineId: "pipe", fill: 4, capacity: 8, payloadStats: {}, readers: [] }],
    },
  };
  const graph = pipelineGraph(state, telemetry, "pipe");
  const byId = new Map(graph.nodes.map((entry) => [entry.id, entry]));
  assert.equal(byId.get("pipe_ingest").metrics.packetsPerSec, 50);
  assert.equal(byId.get("pipe_video_720p").metrics.packetsPerSec, 30);
  assert.equal(byId.get("pipe_video_720p").stageKey, "video:720p");
  assert.equal(byId.get("pipe_output_o").details.feedLagUnits, 7);
  assert.equal(byId.get("pipe_source_rb").details.fill, 4);
  assert.equal(pipelineGraph(state, telemetry, "missing"), null);
});
