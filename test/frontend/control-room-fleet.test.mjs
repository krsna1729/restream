import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { buildFleetModel } = await loadCompiledFrontendModule(
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
  const model = buildFleetModel(
    state(
      [
        { id: "a", name: "A", ingest: INGEST },
        { id: "b", name: "B", ingest: INGEST },
      ],
      [
        output("a1", "a", diagnosed("feed", { code: "noProgress" })),
        output("a2", "a", diagnosed("feed", { code: "noProgress" })),
        output("a3", "a", diagnosed("feed", { code: "noProgress" })),
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
      ["b", "destination", ["b1"]],
      ["b", "destination", ["b2"]],
    ],
  );
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
        output("a3", "a", { convergence: "retrying", flow: "idle" }),
        output("b1", "b", { convergence: "waitingForIngest", flow: "idle" }),
      ],
    ),
    null,
  );
  assert.equal(model.outputsWanted, 3);
  assert.equal(model.outputsDelivering, 1);
  assert.equal(model.pipelinesReceiving, 1);
  // A retrying output keeps A unhealthy; B has no input.
  assert.equal(model.pipelinesHealthy, 0);
  assert.equal(model.egressBps, null, "no rates sampled yet");
});

function telemetry(atMs, ingestBytes, egressBytes) {
  return {
    generatedAt: new Date(atMs).toISOString(),
    ingests: [{ pipelineId: "a", protocol: "srt", uptimeSecs: 1, bytesReceived: ingestBytes, metrics: {} }],
    stages: [],
    egresses: [{ outputId: "a1", bytesOut: egressBytes }],
    activeTranscoderBuffers: 0,
  };
}

test("rates come from two samples and skip counters that restarted", () => {
  const first = telemetry(0, 1_000, 5_000);
  assert.equal(telemetryRates(null, first), null);

  const rates = telemetryRates(first, telemetry(2_000, 251_000, 4_000));
  assert.equal(rates.ingestBps.get("a"), 1_000_000);
  // The egress counter went down: a new session, so no rate rather than a
  // negative one.
  assert.equal(rates.egressBps.has("a1"), false);

  assert.equal(telemetryRates(first, telemetry(0, 2_000, 6_000)), null);
});

test("an unknown or absent workspace opens Fleet", () => {
  assert.equal(parseWorkspace(""), "fleet");
  assert.equal(parseWorkspace("?w=nope"), "fleet");
});
