import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const {
  evidencePacket,
  incidentTimeline,
  rankIncidents,
  timelineMayBeIncomplete,
  updateResolved,
} =
  await loadCompiledFrontendModule("control-room/incidents-model.js");

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

test("a closed alert becomes resolved, newest first and capped; reopening removes it", () => {
  let resolved = updateResolved([], [alert("a"), alert("b")], [alert("b")], 1_000, 2);
  assert.deepEqual(
    resolved.map((r) => [r.alert.id, r.resolvedAtMs]),
    [["a", 1_000]],
  );
  resolved = updateResolved(resolved, [alert("b"), alert("c")], [], 2_000, 2);
  assert.deepEqual(
    resolved.map((r) => r.alert.id),
    ["b", "c"],
  );
  // `b` opens again: it is no longer resolved.
  resolved = updateResolved(resolved, [], [alert("b")], 3_000, 2);
  assert.deepEqual(
    resolved.map((r) => r.alert.id),
    ["c"],
  );
  // Nothing closed or reopened: the same list, so the queue need not redraw.
  assert.equal(updateResolved(resolved, [alert("b")], [alert("b")], 4_000, 2), resolved);
});

test("the evidence packet says what failed, what was ruled out and what to do", () => {
  const packet = evidencePacket(
    alert("dead", {
      evidence: ["phase rtmp_fabric_leaf"],
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
  assert.deepEqual(packet.evidence, ["phase rtmp_fabric_leaf"]);
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
