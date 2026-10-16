import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const {
  NEW_DRAFT,
  changeRequest,
  convergenceTimeline,
  liveConvergence,
  nextStep,
  progress,
  scenarioLoad,
  topologyLines,
  verification,
} = await loadCompiledFrontendModule("control-room/changes-model.js");

test("a change becomes the planner's request; a change to a destination sends only what was filled in", () => {
  const add = { ...NEW_DRAFT, name: " DR ", url: " srt://dr:9000 ", profile: "720p" };
  const update = { ...NEW_DRAFT, kind: "updateOutput", outputId: "o1", name: "", url: "rtmp://new/live/k" };
  const stop = { ...NEW_DRAFT, kind: "stopOutput", outputId: "o2", name: "ignored" };
  assert.deepEqual(changeRequest(" DR drill ", "p1", [add, update, stop]), {
    intent: "DR drill",
    pipelineId: "p1",
    proposedChanges: [
      {
        kind: "addOutput",
        name: "DR",
        url: "srt://dr:9000",
        config: { video: { mode: "preset", preset: "720p" }, audio: { mode: "all" } },
        desiredState: "stopped",
      },
      // An empty name keeps the destination's name.
      { kind: "updateOutput", outputId: "o1", url: "rtmp://new/live/k" },
      { kind: "stopOutput", outputId: "o2" },
    ],
  });
  assert.deepEqual(changeRequest("", "p1", [NEW_DRAFT]).proposedChanges[0].config.video, {
    mode: "source",
  });
});

const plan = (valid) => ({ validation: { valid, errors: [], warnings: [] } });
const op = (status, verificationResult = null) => ({ status, verificationResult });

test("progress follows the plan, then the operation, and marks where a change failed", () => {
  assert.deepEqual(progress(null, null), ["current", "todo", "todo", "todo"]);
  assert.deepEqual(progress(plan(false), null), ["done", "failed", "todo", "todo"]);
  assert.deepEqual(progress(plan(true), null), ["done", "current", "todo", "todo"]);
  for (const status of ["awaitingApproval", "approved", "applying"]) {
    assert.deepEqual(progress(plan(true), op(status)), ["done", "done", "current", "todo"], status);
  }
  assert.deepEqual(progress(plan(true), op("failed")), ["done", "done", "failed", "todo"]);
  assert.deepEqual(progress(plan(true), op("applied")), ["done", "done", "done", "current"]);
  assert.deepEqual(progress(plan(true), op("verificationFailed")), ["done", "done", "done", "failed"]);
  assert.deepEqual(progress(plan(true), op("verified")), ["done", "done", "done", "done"]);
  // The operation, not a later edit of the form, says how far the change came.
  assert.deepEqual(progress(null, op("invalid")), ["done", "failed", "todo", "todo"]);
});

test("each operation offers one next step; a failed verification can run again", () => {
  assert.equal(nextStep("awaitingApproval"), "approve");
  assert.equal(nextStep("approved"), "apply");
  assert.equal(nextStep("applied"), "verify");
  // Verification is a moment's check: a just-started output may connect later.
  assert.equal(nextStep("verificationFailed"), "verify");
  for (const status of ["invalid", "applying", "verified", "failed"]) {
    assert.equal(nextStep(status), null, status);
  }
});

test("the engine's fixed gates decide; per-change checks and alerts are not gates", () => {
  const result = verification(
    op("verificationFailed", {
      gates: [
        { gate: "desiredState", passed: true, detail: "1 of 1" },
        { gate: "deliveryFloor", passed: false, detail: "0 of 1" },
      ],
      checks: [{ kind: "addOutput", outputId: "o9", passed: true, reason: "stopped" }],
      incidentDelta: { delta: 2 },
      graphConvergence: { activeNodes: 5 },
    }),
  );
  assert.deepEqual(
    result.gates.map((gate) => [gate.passed, gate.detail]),
    [
      [true, "1 of 1"],
      [false, "0 of 1"],
    ],
  );
  assert.deepEqual(result.checks, [{ label: "addOutput o9", passed: true, detail: "stopped" }]);
  assert.deepEqual(result.observations, [
    "Open alerts: 2 more than before the change.",
    "Active graph nodes after the change: 5.",
  ]);
  assert.match(verification(op("verified", { incidentDelta: { delta: -1 } })).observations[0], /1 fewer/);
  assert.deepEqual(verification(op("applied")), { gates: [], checks: [], observations: [] });
});

const diff = (before, after, addedStages = [], retiredStages = []) => ({
  pipelineId: "p",
  ingestCodec: "h264",
  before,
  after,
  addedStages,
  retiredStages,
});

test("the topology diff lines up before and after, and marks the stages the change starts", () => {
  const lines = topologyLines(
    diff(
      [
        { stage: "source", protocol: "srt", outputs: 3 },
        { stage: "video:720p", protocol: "rtmp", outputs: 1 },
      ],
      [
        { stage: "video:720p", protocol: "rtmp", outputs: 4 },
        { stage: "video:1080p", protocol: "hls", outputs: 1 },
      ],
      ["video:1080p"],
    ),
  );
  assert.deepEqual(
    lines.map((line) => [line.stage, line.protocol, line.before, line.after, line.newStage]),
    [
      ["video:720p", "rtmp", 1, 4, false],
      ["video:1080p", "hls", 0, 1, true],
      // Gone after: still listed, so the removal shows.
      ["source", "srt", 3, 0, false],
    ],
  );
});

test("the scenario prices only what the change adds; removals are reported, not credited", () => {
  const rows = (n) => [{ stage: "source", protocol: "srt", outputs: n }];
  assert.deepEqual(scenarioLoad(diff(rows(2), rows(5), ["a"])), {
    addOutputs: 3,
    addStages: 1,
    removedOutputs: 0,
  });
  assert.deepEqual(scenarioLoad(diff(rows(5), rows(2))), {
    addOutputs: 0,
    addStages: 0,
    removedOutputs: 3,
  });
});

const T0 = Date.parse("2026-10-16T10:00:00Z");
const at = (ms) => new Date(T0 + ms).toISOString();

test("the convergence timeline merges audit events and output changes in time order", () => {
  const operation = {
    auditLog: [
      { at: at(0), kind: "created", summary: "op made" },
      { at: at(5000), kind: "applyCompleted", summary: "applied" },
      { at: "garbled", kind: "approved", summary: "dropped" },
    ],
    stateTransitions: [
      { at: at(4000), kind: "addOutput", pipelineId: "p", outputId: "o1", from: null, to: { desiredState: "running" } },
      {
        at: at(4500),
        kind: "stopOutput",
        pipelineId: "p",
        outputId: "o2",
        from: { desiredState: "running" },
        to: { desiredState: "stopped" },
      },
      { at: at(4800), kind: "removeOutput", pipelineId: "p", outputId: "o3", from: { desiredState: "stopped" }, to: null },
    ],
  };
  assert.deepEqual(
    convergenceTimeline(operation).map((step) => [step.offsetMs, step.label]),
    [
      [0, "created: op made"],
      [4000, "o1 created, desired running"],
      [4500, "o2 desired running → stopped"],
      [4800, "o3 removed"],
      [5000, "applyCompleted: applied"],
    ],
  );

  const live = liveConvergence(operation, [
    { id: "o1", name: "cdn-a", convergence: "starting" },
    { id: "o2", name: "cdn-b", convergence: "converged" },
    { id: "o9", name: "untouched", convergence: "retrying" },
  ]);
  // o3 was removed; o9 was not part of the change.
  assert.deepEqual(live, {
    converged: 1,
    total: 2,
    states: [
      ["cdn-a", "starting"],
      ["cdn-b", "converged"],
    ],
  });
});
