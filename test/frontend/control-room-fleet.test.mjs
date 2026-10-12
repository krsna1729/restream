import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { buildFleetModel, pipelineHealth } = await loadCompiledFrontendModule(
  "control-room/fleet-model.js",
);
const { telemetryRates } = await loadCompiledFrontendModule(
  "control-room/rates.js",
);
const { parseWorkspace } = await loadCompiledFrontendModule(
  "control-room/route.js",
);

const INGEST = { inputId: "in", protocol: "srt", videoCodec: "h264" };

function output(id, pipelineId, overrides = {}) {
  return {
    id,
    pipelineId,
    name: id,
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

function diagnosed(layer, cause) {
  return { diagnosis: { layer, cause, ruledOut: [] }, flow: "stalled" };
}

function state(pipelines, outputs) {
  return { epoch: "e", revision: 1, pipelines, outputs };
}

test("one upstream cause is one attention item covering every output it stalls", () => {
  const blocked720 = { code: "stageNotProducing", stage: "720p", phase: "starting" };
  const blocked1080 = { code: "stageNotProducing", stage: "1080p", phase: "starting" };
  const model = buildFleetModel(
    state(
      [
        { id: "a", name: "A", ingest: INGEST },
        { id: "b", name: "B", ingest: INGEST },
      ],
      [
        output("a1", "a", diagnosed("feed", blocked720)),
        output("a2", "a", diagnosed("feed", blocked720)),
        output("a3", "a", diagnosed("feed", blocked720)),
        // Another blocked stage in the same pipeline is another cause.
        output("a4", "a", diagnosed("feed", blocked1080)),
        // Two destination faults of one cause stay separate: each is its own
        // remote.
        output("b1", "b", diagnosed("destination", { code: "retriesExhausted" })),
        output("b2", "b", diagnosed("destination", { code: "retriesExhausted" })),
      ],
    ),
    null,
  );
  assert.deepEqual(
    model.attention.map((item) => [item.pipelineId, item.layer, item.outputs]),
    [
      ["a", "feed", ["a1", "a2", "a3"]],
      ["a", "feed", ["a4"]],
      ["b", "destination", ["b1"]],
      ["b", "destination", ["b2"]],
    ],
  );
  assert.equal(model.attention[1].cause.stage, "1080p");
  assert.deepEqual(
    model.pipelines.map((row) => row.id),
    ["a", "b"],
    "the pipeline with more degraded outputs comes first",
  );
});

test("equal blast radius ranks the upstream layer first", () => {
  const model = buildFleetModel(
    state(
      [
        { id: "a", name: "A", ingest: INGEST },
        { id: "b", name: "B", ingest: null },
      ],
      [
        output("a1", "a", diagnosed("destination", { code: "noProgress" })),
        output("b1", "b", diagnosed("ingest", { code: "noPublisher" })),
      ],
    ),
    null,
  );
  assert.deepEqual(
    model.attention.map((item) => item.layer),
    ["ingest", "destination"],
  );
});

test("delivery counts only wanted outputs that converged and flow", () => {
  const model = buildFleetModel(
    state(
      [
        { id: "a", name: "A", ingest: INGEST },
        { id: "b", name: "B", ingest: null },
      ],
      [
        output("a1", "a"),
        output("a2", "a", { desired: "stopped", flow: "idle" }),
        output("a3", "a", { convergence: "starting", flow: "idle" }),
        // Delivering with a diagnosis: counted once, as attention.
        output("a4", "a", {
          ...diagnosed("destination", { code: "backpressured", stalled: false }),
          flow: "flowing",
        }),
        output("b1", "b", diagnosed("ingest", { code: "noPublisher" })),
      ],
    ),
    null,
  );
  assert.equal(model.outputsWanted, 4);
  assert.equal(model.outputsDelivering, 2);
  assert.equal(model.outputsPending, 1, "a starting output is neither delivering nor diagnosed");
  assert.equal(model.pipelinesReceiving, 1);
  // A starting output keeps A unhealthy; B has no input.
  assert.equal(model.pipelinesHealthy, 0);
  assert.equal(model.egressBps, null, "no rates sampled yet");
});

test("a row is healthy only when every wanted output delivers", () => {
  const row = { attention: 0, pending: 0, source: { protocol: "srt", videoCodec: null } };
  assert.equal(pipelineHealth({ ...row, attention: 1, pending: 2 }).label, "1 degraded");
  assert.equal(pipelineHealth({ ...row, pending: 2 }).label, "2 starting");
  assert.equal(pipelineHealth({ ...row, source: null }).label, "idle");
  assert.equal(pipelineHealth(row).label, "healthy");
});

function telemetry(atMs, ingestBytes, egressBytes, streamKey = "k1") {
  return {
    generatedAt: new Date(atMs).toISOString(),
    ingests: [{ pipelineId: "a", protocol: "srt", streamKey, uptimeSecs: 1, bytesReceived: ingestBytes, metrics: {} }],
    stages: [],
    egresses: [{ outputId: "a1", bytesOut: egressBytes }],
    activeTranscoderBuffers: 0,
  };
}

test("rates come from two samples of one session", () => {
  const first = telemetry(0, 1_000, 5_000);
  assert.equal(telemetryRates(null, first), null);

  const rates = telemetryRates(first, telemetry(2_000, 251_000, 4_000));
  assert.equal(rates.ingestBps.get("a"), 1_000_000);
  // The egress counter went down: a new session, so no rate rather than a
  // negative one.
  assert.equal(rates.egressBps.has("a1"), false);

  // After a failover the pipeline's counter is another input's, even when
  // it is higher.
  const failover = telemetryRates(first, telemetry(2_000, 9_000_000, 6_000, "k2"));
  assert.equal(failover.ingestBps.has("a"), false);

  assert.equal(telemetryRates(first, telemetry(0, 2_000, 6_000)), null);
});

test("an unknown or absent workspace opens Fleet", () => {
  assert.equal(parseWorkspace(""), "fleet");
  assert.equal(parseWorkspace("?w=nope"), "fleet");
});
