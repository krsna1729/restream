import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { buildMediaGraph, buildDestinationRows } = await loadCompiledFrontendModule(
  "control-room/operate-model.js",
);
const { buildFleetModel, filterPipelineRows } = await loadCompiledFrontendModule(
  "control-room/fleet-model.js",
);

function output(id, overrides = {}) {
  return {
    id,
    pipelineId: "p",
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

const DEGRADED = {
  diagnosis: { layer: "destination", cause: { code: "noProgress" }, ruledOut: [] },
  flow: "stalled",
};

function node(id, type, details = null) {
  return { id, type, label: id, active: true, details, metrics: null };
}

function edge(from, to) {
  return { from, to, label: "" };
}

// The server's shape: one node per output named `<pipeline>_output_<id>`.
function graph(nodes, edges) {
  return { pipelineId: "p", nodes, edges };
}

test("a shared stage is drawn once with its consumers; destinations group by protocol", () => {
  const outputs = [
    output("a"),
    output("b", DEGRADED),
    output("c", { protocol: "srt" }),
  ];
  const columns = buildMediaGraph(
    graph(
      [
        node("ingest", "ingest"),
        node("demux", "demux"),
        node("ring", "ring_buffer"),
        node("p_output_a", "egress"),
        node("p_output_b", "egress"),
        node("p_output_c", "egress"),
        node("p_output_ghost", "egress"),
      ],
      [
        edge("ingest", "demux"),
        edge("demux", "ring"),
        edge("ring", "p_output_a"),
        edge("ring", "p_output_b"),
        edge("ring", "p_output_c"),
        edge("ring", "p_output_ghost"),
      ],
    ),
    outputs,
  );

  assert.deepEqual(
    columns.map((column) => column.stages.map((stage) => stage.id)),
    [["ingest"], ["demux"], ["ring"], []],
  );
  assert.equal(columns[2].stages[0].consumers, 4);
  assert.deepEqual(
    columns[3].groups.map(({ protocol, outputs, delivering, attention }) => ({
      protocol,
      outputs,
      delivering,
      attention,
    })),
    [
      { protocol: "rtmp", outputs: 2, delivering: 1, attention: 1 },
      { protocol: "srt", outputs: 1, delivering: 1, attention: 0 },
      // An egress node the runtime state does not know.
      { protocol: null, outputs: 1, delivering: 0, attention: 0 },
    ],
  );
});

test("a stage sits right of the longest path into it; a cycle does not hang", () => {
  const columns = buildMediaGraph(
    graph(
      [node("ring", "ring_buffer"), node("scale", "transcode"), node("mux", "mux")],
      // `mux` reads the ring directly and through `scale`.
      [edge("ring", "scale"), edge("scale", "mux"), edge("ring", "mux")],
    ),
    [],
  );
  assert.deepEqual(
    columns.map((column) => column.stages.map((stage) => stage.id)),
    [["ring"], ["scale"], ["mux"]],
  );

  const cyclic = buildMediaGraph(
    graph([node("x", "demux"), node("y", "demux")], [edge("x", "y"), edge("y", "x")]),
    [],
  );
  assert.equal(cyclic.flatMap((column) => column.stages).length, 2);
});

test("destinations: degraded first, then wanted but not delivering, then by name", () => {
  const rows = buildDestinationRows(
    "p",
    [
      output("zulu"),
      output("alpha", { convergence: "starting", flow: "idle", actual: null }),
      output("stopped", { desired: "stopped", convergence: "converged", flow: "idle", actual: null }),
      output("mike", DEGRADED),
      output("other-pipeline", { pipelineId: "q" }),
    ],
    graph(
      [
        node("p_output_zulu", "egress", { feedLagUnits: 3 }),
        node("p_output_alpha", "egress", null),
      ],
      [],
    ),
  );
  assert.deepEqual(
    rows.map((row) => row.output.id),
    ["mike", "alpha", "stopped", "zulu"],
  );
  const zulu = rows.find((row) => row.output.id === "zulu");
  assert.equal(zulu.feedLagUnits, 3);
  // No running egress: no number rather than a zero.
  const alpha = rows.find((row) => row.output.id === "alpha");
  assert.equal(alpha.feedLagUnits, null);
  assert.equal(alpha.delivering, false);
});

test("the inventory filters by name or id in any case, and by health", () => {
  const ingest = { inputId: "in", protocol: "srt", videoCodec: "h264" };
  const model = buildFleetModel(
    {
      epoch: "e",
      revision: 1,
      pipelines: [
        { id: "pipe_main", name: "Temple Main", ingest },
        { id: "pipe_b", name: "Stage B", ingest },
        { id: "pipe_idle", name: "Idle", ingest: null },
      ],
      outputs: [
        output("m1", { pipelineId: "pipe_main" }),
        output("m2", { pipelineId: "pipe_main", protocol: "srt" }),
        output("b1", { pipelineId: "pipe_b", ...DEGRADED }),
      ],
    },
    null,
  );
  const ids = (rows) => rows.map((row) => row.id).sort();
  assert.deepEqual(ids(filterPipelineRows(model.pipelines, "  TEMPLE ", "all")), ["pipe_main"]);
  assert.deepEqual(ids(filterPipelineRows(model.pipelines, "pipe_", "all")), [
    "pipe_b",
    "pipe_idle",
    "pipe_main",
  ]);
  assert.deepEqual(ids(filterPipelineRows(model.pipelines, "", "warning")), ["pipe_b"]);
  assert.deepEqual(ids(filterPipelineRows(model.pipelines, "", "idle")), ["pipe_idle"]);
  assert.deepEqual(ids(filterPipelineRows(model.pipelines, "stage", "ok")), []);

  const main = model.pipelines.find((row) => row.id === "pipe_main");
  assert.deepEqual([...main.protocols], [["rtmp", 1], ["srt", 1]]);
});
