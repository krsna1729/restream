import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { degradedTotal, takeFleetSample } = await loadCompiledFrontendModule(
  "control-room/fleet-history.js",
);
const { worstFirst } = await loadCompiledFrontendModule("control-room/fleet-model.js");

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

function diagnosed(layer) {
  return { diagnosis: { layer, cause: { code: "noProgress" }, ruledOut: [] }, flow: "stalled" };
}

test("a fleet sample counts wanted outputs only, delivering and diagnosed by layer", () => {
  const sample = takeFleetSample({
    atMs: 7,
    outputs: [
      output("a"),
      output("b", diagnosed("destination")),
      output("c", diagnosed("shard")),
      output("d", diagnosed("destination")),
      // Not wanted: neither delivering nor degraded counts it.
      output("e", { desired: "stopped", ...diagnosed("feed") }),
    ],
    capacity: { projectedUtilization: 0.3 },
  });
  assert.equal(sample.atMs, 7);
  assert.equal(sample.deliveringPercent, 25);
  assert.deepEqual(sample.degraded, { ingest: 0, feed: 0, shard: 1, destination: 2 });
  assert.equal(degradedTotal(sample), 3);
  assert.equal(Math.round(sample.headroomPercent), 70);
});

test("a source with nothing to say is a gap, never a zero", () => {
  // The stream is down and the metrics read failed.
  const blind = takeFleetSample({ atMs: 0, outputs: null, capacity: null });
  assert.equal(blind.headroomPercent, null);
  assert.equal(blind.deliveringPercent, null);
  assert.equal(blind.degraded, null);
  assert.equal(degradedTotal(blind), null);

  // Nothing wanted: no delivery share, but zero degraded is a fact.
  const idle = takeFleetSample({ atMs: 0, outputs: [output("a", { desired: "stopped" })], capacity: null });
  assert.equal(idle.deliveringPercent, null);
  assert.equal(degradedTotal(idle), 0);

  // Over the projected limit: no headroom, not a negative one.
  assert.equal(
    takeFleetSample({ atMs: 0, outputs: [], capacity: { projectedUtilization: 1.4 } }).headroomPercent,
    0,
  );
});

test("the fleet table puts degraded first, then starting, healthy and idle", () => {
  const row = (name, overrides) => ({
    id: name,
    name,
    source: { protocol: "srt", videoCodec: "h264" },
    wanted: 1,
    attention: 0,
    pending: 0,
    ...overrides,
  });
  const rows = [
    row("idle", { source: null, wanted: 0 }),
    row("healthy-b"),
    row("starting", { pending: 1 }),
    row("healthy-a"),
    row("healthy-big", { wanted: 9 }),
    row("degraded", { attention: 1 }),
  ];
  assert.deepEqual(
    worstFirst(rows).map((r) => r.name),
    ["degraded", "starting", "healthy-big", "healthy-a", "healthy-b", "idle"],
  );
  // The caller's rows keep their order.
  assert.equal(rows[0].name, "idle");
});
