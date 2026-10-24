import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const fleetModel = await loadCompiledFrontendModule("control-room/fleet-model.js");
const { attentionGroups, buildFleetModel, pipelineHealth } = fleetModel;
const { appendTrends, telemetryRates } = await loadCompiledFrontendModule(
  "control-room/rates.js",
);
const { parseSearch, placeFromSearch, placeSearch, stringifySearch } =
  await loadCompiledFrontendModule("control-room/route.js");

const INGEST = { inputId: "in", protocol: "srt", videoCodec: "h264" };

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

test("the healthy-pipeline count follows the row badge", () => {
  const model = buildFleetModel(
    state(
      [
        { id: "a", name: "A", ingest: INGEST },
        { id: "b", name: "B", ingest: INGEST },
      ],
      [
        // Every output of A delivers, but one is diagnosed: not healthy.
        output("a1", "a", {
          ...diagnosed("destination", { code: "backpressured", stalled: false }),
          flow: "flowing",
        }),
        output("b1", "b"),
      ],
    ),
    null,
  );
  assert.deepEqual(
    model.pipelines.map((row) => [row.id, pipelineHealth(row).tone]),
    [
      ["a", "warning"],
      ["b", "ok"],
    ],
  );
  assert.equal(model.pipelinesHealthy, 1);
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

test("a row trend keeps the last samples and restarts across a gap", () => {
  let trends = new Map();
  for (let index = 1; index <= 5; index += 1) {
    trends = appendTrends(trends, new Map([["a", index], ["b", index * 10]]), 3);
  }
  assert.deepEqual(trends.get("a"), [3, 4, 5], "oldest first, the last three kept");
  assert.deepEqual(trends.get("b"), [30, 40, 50]);
  // "b" had no rate this sample (a new session): its line starts again.
  trends = appendTrends(trends, new Map([["a", 6]]), 3);
  assert.deepEqual(trends.get("a"), [4, 5, 6]);
  assert.equal(trends.has("b"), false);
  trends = appendTrends(trends, new Map([["b", 7]]), 3);
  assert.deepEqual(trends.get("b"), [7]);
});

test("an unknown or absent workspace opens Fleet; the pipeline is kept", () => {
  assert.equal(placeFromSearch({}).workspace, "fleet");
  assert.deepEqual(placeFromSearch({ w: "nope", p: "p1" }), {
    workspace: "fleet",
    pipelineId: "p1",
  });
  assert.deepEqual(placeFromSearch({ w: "operate", p: "" }), {
    workspace: "operate",
    pipelineId: null,
  });
});

test("a place without a pipeline drops `p`; other parameters stay, after them", () => {
  assert.deepEqual(
    placeSearch(
      { w: "operate", p: "p1", debug: "1" },
      { workspace: "fleet", pipelineId: null },
    ),
    { w: "fleet", debug: "1" },
  );
  const search = placeSearch({}, { workspace: "operate", pipelineId: "a b" });
  assert.deepEqual(placeFromSearch(search), {
    workspace: "operate",
    pipelineId: "a b",
  });
  // The router writes `w` then `p`, so a shared link reads as the e2e asserts.
  assert.deepEqual(
    Object.keys(
      placeSearch({ debug: "1" }, { workspace: "operate", pipelineId: "p1" }),
    ),
    ["w", "p", "debug"],
  );
});

test("a query string stays plain strings through the round trip", () => {
  // A numeric-looking id is a string, not JSON: `p=123` names pipeline 123.
  // (A JSON-parsing parseSearch would make this a number and drop it.)
  assert.deepEqual(placeFromSearch(parseSearch("?w=operate&p=123")), {
    workspace: "operate",
    pipelineId: "123",
  });
  // The first value of a repeated parameter wins, as `get` did before.
  assert.deepEqual(parseSearch("?w=operate&w=media"), { w: "operate" });
  // A name the object prototype also has stays an own parameter.
  assert.deepEqual(parseSearch("?toString=1"), { toString: "1" });
  assert.deepEqual(Object.keys(parseSearch("?__proto__=x")), ["__proto__"]);
  // ...and survives a move to another place.
  assert.equal(
    stringifySearch(
      placeSearch(parseSearch("?__proto__=x"), {
        workspace: "fleet",
        pipelineId: null,
      }),
    ),
    "?w=fleet&__proto__=x",
  );
  assert.equal(
    stringifySearch(
      placeSearch(parseSearch("?debug=1&p=123&w=operate"), {
        workspace: "operate",
        pipelineId: "123",
      }),
    ),
    "?w=operate&p=123&debug=1",
  );
  const spaced = placeSearch({}, { workspace: "operate", pipelineId: "a b" });
  assert.deepEqual(placeFromSearch(parseSearch(stringifySearch(spaced))), {
    workspace: "operate",
    pipelineId: "a b",
  });
  assert.equal(
    stringifySearch(
      placeSearch({ p: "", debug: "1" }, { workspace: "fleet", pipelineId: null }),
    ),
    "?w=fleet&debug=1",
  );
  assert.equal(stringifySearch({}), "");
});

test("attention groups roll causes up across pipelines, largest first", () => {
  const rejected = { code: "rejected", phase: "connect", error: "refused" };
  const model = buildFleetModel(
    state(
      [
        { id: "a", name: "A", ingest: INGEST, standbyInputs: 0, recording: false },
        { id: "b", name: "B", ingest: null, standbyInputs: 0, recording: false },
        { id: "c", name: "C", ingest: INGEST, standbyInputs: 0, recording: false },
      ],
      [
        output("a1", "a", diagnosed("destination", rejected)),
        output("c1", "c", diagnosed("destination", { ...rejected, error: "timeout" })),
        output("b1", "b", diagnosed("ingest", { code: "noPublisher" })),
        output("b2", "b", diagnosed("ingest", { code: "noPublisher" })),
        output("b3", "b", diagnosed("ingest", { code: "noPublisher" })),
      ],
    ),
    null,
  );
  const groups = attentionGroups(model.attention);
  assert.deepEqual(
    groups.map((group) => [group.key, group.outputs, group.pipelines, group.items.length]),
    [
      ["ingest:noPublisher", 3, 1, 1],
      // Two different errors are two items but one kind of trouble.
      ["destination:rejected", 2, 2, 2],
    ],
  );
  assert.equal(groups[1].title, "Destination rejected the stream");
});

test("a logs link carries its destination, and any other move drops it", () => {
  assert.deepEqual(placeFromSearch({ w: "logs", p: "p1", o: "out_1" }), { workspace: "logs", pipelineId: "p1", outputId: "out_1" });
  assert.deepEqual(placeSearch({ x: "1" }, { workspace: "logs", pipelineId: "p1", outputId: "out_1" }), { w: "logs", p: "p1", o: "out_1", x: "1" });
  assert.deepEqual(placeSearch({ w: "logs", p: "p1", o: "out_1" }, { workspace: "operate", pipelineId: "p1" }), { w: "operate", p: "p1" });
});

test("a replay window rides in the URL and a following one has no end", () => {
  assert.deepEqual(placeFromSearch({ w: "stability", p: "p1", from: "1000", to: "5000" }).replay, { fromMs: 1000, toMs: 5000 });
  assert.deepEqual(placeFromSearch({ w: "stability", p: "p1", from: "1000" }).replay, { fromMs: 1000, toMs: null });
  assert.equal(placeFromSearch({ w: "stability", from: "soon" }).replay, undefined);
  assert.deepEqual(placeSearch({}, { workspace: "stability", pipelineId: "p1", replay: { fromMs: 1000, toMs: null } }), { w: "stability", p: "p1", from: "1000" });
  assert.deepEqual(placeSearch({ w: "stability", from: "1", to: "2" }, { workspace: "fleet", pipelineId: null }), { w: "fleet" });
});

test("headroom reads as about how many more destinations fit", () => {
  const { roomFor } = fleetModel;
  assert.equal(roomFor(0.4, 100), 100, "half the safe load used: as many again");
  assert.equal(roomFor(0.3, 37), 61, "rounded to two figures");
  assert.equal(roomFor(0.79, 10), 0);
  assert.equal(roomFor(0.9, 10), 0, "past the safe load: no room, never negative");
  assert.equal(roomFor(0.2, 0), null, "nothing running to average");
});
