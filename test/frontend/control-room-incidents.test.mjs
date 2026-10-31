import assert from "node:assert/strict";
import test from "node:test";

import {
  evidenceExport,
  evidencePacket,
  fleetSpanSettled,
  incidentMarks,
  incidentTimeline,
  rankIncidents,
  resolvedMayBeIncomplete,
  timelineMayBeIncomplete,
  visibleResolved,
} from "../../web/ts/control-room/incidents-model.ts";

function alert(id, overrides = {}) {
  return {
    id,
    severity: "warning",
    scope: "output",
    pipelineId: "p",
    title: id,
    cause: "",
    evidence: [],
    recommendedAction: "",
    generatedAt: "",
    firstSeen: "2026-10-12T10:00:00Z",
    ...overrides,
  };
}

test("incidents rank critical first, then the widest scope, then the oldest", () => {
  const ranked = rankIncidents([
    alert("output-new", { firstSeen: "2026-10-12T10:05:00Z" }),
    alert("output-old", { firstSeen: "2026-10-12T10:01:00Z" }),
    // Newest, but it affects the whole pipeline.
    alert("pipeline", { scope: "pipeline", firstSeen: "2026-10-12T10:10:00Z" }),
    alert("critical-output", { severity: "critical" }),
    alert("unknown-start", { firstSeen: undefined }),
  ]);
  assert.deepEqual(
    ranked.map((a) => a.id),
    ["critical-output", "pipeline", "output-old", "output-new", "unknown-start"],
  );
});

test("resolved: an alert open again leaves, and a repeat id shows its latest closing", () => {
  const resolved = [
    { alert: alert("a"), resolvedAt: "2026-10-12T10:05:00Z" },
    { alert: alert("b"), resolvedAt: "2026-10-12T10:04:00Z" },
    { alert: alert("a"), resolvedAt: "2026-10-12T10:02:00Z" },
  ];
  assert.deepEqual(
    visibleResolved(resolved, []).map((r) => [r.alert.id, r.resolvedAt]),
    [
      ["a", "2026-10-12T10:05:00Z"],
      ["b", "2026-10-12T10:04:00Z"],
    ],
  );
  assert.deepEqual(
    visibleResolved(resolved, [alert("a")]).map((r) => r.alert.id),
    ["b"],
  );
});

test("a resolved window reaching past the kept history may be missing some", () => {
  const from = "2026-10-12T10:00:00Z";
  assert.equal(resolvedMayBeIncomplete(Date.parse(from), from), true, "exclusive bound");
  assert.equal(resolvedMayBeIncomplete(Date.parse(from) + 1, from), false);
  assert.equal(resolvedMayBeIncomplete(0, undefined), false);
});

test("the chart marks the open and, once resolved, the close", () => {
  const opened = alert("a", { firstSeen: "2026-10-12T10:00:00Z" });
  assert.deepEqual(incidentMarks(opened, null), [Date.parse("2026-10-12T10:00:00Z")]);
  assert.deepEqual(incidentMarks(opened, "2026-10-12T10:03:00Z"), [
    Date.parse("2026-10-12T10:00:00Z"),
    Date.parse("2026-10-12T10:03:00Z"),
  ]);
  assert.deepEqual(incidentMarks(alert("b", { firstSeen: undefined }), null), []);
});

test("the export keeps the samples from the timeline's start to the close", () => {
  const at = (iso) => Date.parse(iso);
  const sample = (iso) => ({ atMs: at(iso), headroomPercent: 50, deliveringPercent: 100, degraded: null });
  const history = [
    sample("2026-10-12T09:58:59Z"), // before the minute of lead-up
    sample("2026-10-12T09:59:00Z"),
    sample("2026-10-12T10:02:00Z"),
    sample("2026-10-12T10:03:01Z"), // after it closed
  ];
  const record = evidenceExport({
    alert: alert("a", { firstSeen: "2026-10-12T10:00:00Z" }),
    resolvedAt: "2026-10-12T10:03:00Z",
    events: [event(1, "2026-10-12T09:58:00Z"), event(2, "2026-10-12T10:01:00Z")],
    history,
    nowMs: at("2026-10-12T10:10:00Z"),
  });
  assert.equal(record.status, "resolved");
  assert.equal(record.exportedAt, "2026-10-12T10:10:00.000Z");
  assert.deepEqual(
    record.signals.map((s) => s.atMs),
    [at("2026-10-12T09:59:00Z"), at("2026-10-12T10:02:00Z")],
  );
  assert.deepEqual(record.timeline, { events: [event(2, "2026-10-12T10:01:00Z")], mayBeIncomplete: false });

  // Open: the window runs to now; no timeline loaded is said, not faked.
  const open = evidenceExport({
    alert: alert("a", { firstSeen: "2026-10-12T10:00:00Z" }),
    resolvedAt: null,
    events: null,
    history,
    nowMs: at("2026-10-12T10:10:00Z"),
  });
  assert.equal(open.status, "open");
  assert.equal(open.timeline, null);
  assert.equal(open.signals.length, 3);
});

test("the evidence packet says what failed, what was ruled out and what to do", () => {
  const packet = evidencePacket(
    alert("dead", {
      evidence: [{ signal: "phase rtmp_fabric_leaf", confidence: "measured" }],
      recommendedAction: "Check the destination.",
      diagnosis: {
        layer: "destination",
        cause: { code: "rejected", phase: "connect", error: "refused" },
        ruledOut: ["ingest", "feed"],
      },
    }),
  );
  assert.match(packet.failed, /^destination/);
  assert.equal(packet.notFailed.length, 2);
  assert.match(packet.notFailed[0], /^ingest/);
  assert.match(packet.notFailed[1], /^feed/);
  assert.deepEqual(packet.evidence, [{ signal: "phase rtmp_fabric_leaf", confidence: "measured" }]);
  assert.equal(packet.action, "Check the destination.");

  const undiagnosed = evidencePacket(alert("no-publisher", { scope: "pipeline" }));
  assert.equal(undiagnosed.failed, null);
  assert.deepEqual(undiagnosed.notFailed, []);
});

function event(seq, timestamp, overrides = {}) {
  return { seq, timestamp, kind: "egressFailed", pipelineId: "p", ...overrides };
}

test("the timeline: this pipeline and output, from a minute before it opened, oldest first", () => {
  const events = [
    event(5, "2026-10-12T10:00:30Z", { outputId: "o" }),
    event(1, "2026-10-12T09:58:00Z", { outputId: "o" }), // before the lead
    event(2, "2026-10-12T09:59:30Z", { outputId: "o" }),
    event(3, "2026-10-12T10:00:10Z", { outputId: "other" }),
    event(4, "2026-10-12T10:00:20Z", { kind: "ingestStarted" }), // pipeline-wide
    event(6, "2026-10-12T10:00:40Z", { pipelineId: "q", outputId: "o" }),
  ];
  assert.deepEqual(
    incidentTimeline(alert("x", { outputId: "o" }), events).map((e) => e.seq),
    [2, 4, 5],
  );
  // A pipeline alert keeps every output of its pipeline.
  assert.deepEqual(
    incidentTimeline(alert("y", { scope: "pipeline" }), events).map((e) => e.seq),
    [2, 3, 4, 5],
  );
  // No open time: every event of the pipeline.
  assert.deepEqual(
    incidentTimeline(alert("z", { scope: "pipeline", firstSeen: undefined }), events).map(
      (e) => e.seq,
    ),
    [1, 2, 3, 4, 5],
  );
});

test("the timeline is complete only when the log reaches back to its start", () => {
  const opened = alert("x", { firstSeen: "2026-10-12T10:00:00Z" });
  const atStart = event(1, "2026-10-12T09:59:00Z");
  const inWindow = event(2, "2026-10-12T09:59:30Z");
  const after = event(3, "2026-10-12T10:00:30Z");
  // The oldest fetched event is at the window start: nothing before it matters.
  assert.equal(timelineMayBeIncomplete(opened, [after, atStart]), false);
  // A quiet minute before the open still proves coverage if older events exist.
  assert.equal(timelineMayBeIncomplete(opened, [event(0, "2026-10-12T09:00:00Z"), after]), false);
  // The log starts inside the window, or holds nothing: the lead-up may be gone.
  assert.equal(timelineMayBeIncomplete(opened, [inWindow, after]), true);
  assert.equal(timelineMayBeIncomplete(opened, []), true);
});

test("a closed span settles on the server's newest sample, not the browser's clock", () => {
  const closedAt = 10_000;
  const window = (newestMs, completeFromMs = 0) => ({
    samples: [],
    completeFromMs,
    intervalMs: 5_000,
    newestMs,
  });
  // The server has not sampled past the close yet: more may come.
  assert.equal(fleetSpanSettled(window(null), closedAt), false);
  assert.equal(fleetSpanSettled(window(9_999), closedAt), false);
  // It has: every sample in the span was taken.
  assert.equal(fleetSpanSettled(window(10_000), closedAt), true);
  // The span aged out of what the server keeps: nothing more can come.
  assert.equal(fleetSpanSettled(window(null, 10_000), closedAt), true);
});
