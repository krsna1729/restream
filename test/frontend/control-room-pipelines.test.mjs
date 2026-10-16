import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { buildFleetModel, filterPipelineRows, OPERATIONAL } =
  await loadCompiledFrontendModule("control-room/fleet-model.js");
const { protocolMix, sharedStages } = await loadCompiledFrontendModule(
  "control-room/pipelines-model.js",
);

function pipeline(id, overrides = {}) {
  return {
    id,
    name: id.toUpperCase(),
    ingest: { inputId: "in", protocol: "srt", videoCodec: "h264", bonded: false },
    standbyInputs: 0,
    recording: false,
    ...overrides,
  };
}

function output(id, overrides = {}) {
  return {
    id,
    pipelineId: "a",
    name: id,
    protocol: "rtmp",
    shard: null,
    stage: null,
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

test("the protocol mix counts every output fleet-wide, the largest first", () => {
  const model = buildFleetModel(
    {
      epoch: "e",
      revision: 1,
      pipelines: [pipeline("a"), pipeline("b")],
      outputs: [
        output("a1"),
        output("a2", { protocol: "srt" }),
        output("b1", { pipelineId: "b", protocol: "srt" }),
        // A stopped output still counts: the mix is configuration, not load.
        output("b2", { pipelineId: "b", protocol: "hls", desired: "stopped" }),
      ],
    },
    null,
  );
  assert.deepEqual(protocolMix(model.pipelines), [
    { protocol: "srt", outputs: 2 },
    { protocol: "hls", outputs: 1 },
    { protocol: "rtmp", outputs: 1 },
  ]);
});

test("outputs on one stage form a sharing group; a lone stage shares nothing", () => {
  const pipelines = [pipeline("a"), pipeline("b")];
  const outputs = [
    output("a1", { stage: "a:video:720p" }),
    output("a2", { stage: "a:video:720p" }),
    output("a3", { stage: "a:video:720p" }),
    output("a4", { stage: "a:source" }),
    output("b1", { pipelineId: "b", stage: "b:source" }),
    output("b2", { pipelineId: "b", stage: "b:source" }),
    // Not running: reads no stage.
    output("b3", { pipelineId: "b", stage: null, actual: null }),
  ];
  assert.deepEqual(sharedStages(pipelines, outputs), {
    groups: [
      { pipelineId: "a", pipelineName: "A", kind: "video:720p", outputs: 3 },
      { pipelineId: "b", pipelineName: "B", kind: "source", outputs: 2 },
    ],
    sharedOutputs: 5,
    stagedOutputs: 6,
  });
  assert.deepEqual(sharedStages(pipelines, [output("a1", { stage: "a:source" })]), {
    groups: [],
    sharedOutputs: 0,
    stagedOutputs: 1,
  });
});

test("each operational card selects only its pipelines and composes with search", () => {
  const model = buildFleetModel(
    {
      epoch: "e",
      revision: 1,
      pipelines: [
        pipeline("plain"),
        pipeline("standby", { standbyInputs: 1 }),
        pipeline("bonded", {
          ingest: { inputId: "in", protocol: "srt", videoCodec: null, bonded: true },
        }),
        pipeline("recording", { recording: true }),
        pipeline("idle", { ingest: null, standbyInputs: 2 }),
        pipeline("hurt"),
      ],
      outputs: [
        output("h1", {
          pipelineId: "hurt",
          flow: "stalled",
          diagnosis: { layer: "destination", cause: { code: "noProgress" }, ruledOut: [] },
        }),
      ],
    },
    null,
  );
  const ids = (rows) => rows.map((row) => row.id).sort();
  const only = (card) => ids(filterPipelineRows(model.pipelines, "", "all", card));
  assert.deepEqual(only("attention"), ["hurt"]);
  // A standby input counts while the pipeline has no selected ingest.
  assert.deepEqual(only("standby"), ["idle", "standby"]);
  assert.deepEqual(only("bonded"), ["bonded"]);
  assert.deepEqual(only("recording"), ["recording"]);
  assert.equal(ids(filterPipelineRows(model.pipelines, "", "all", null)).length, 6);
  assert.deepEqual(ids(filterPipelineRows(model.pipelines, "IDLE", "all", "standby")), ["idle"]);
  assert.deepEqual(ids(filterPipelineRows(model.pipelines, "", "ok", "standby")), ["standby"]);
  assert.equal(model.pipelines.filter(OPERATIONAL.standby).length, 2);
});
