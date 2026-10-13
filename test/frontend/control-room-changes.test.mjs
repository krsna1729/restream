import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { NEW_DRAFT, changeRequest, nextStep, progress, verification } =
  await loadCompiledFrontendModule("control-room/changes-model.js");

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

test("each operation offers one next step, and none once finished", () => {
  assert.equal(nextStep("awaitingApproval"), "approve");
  assert.equal(nextStep("approved"), "apply");
  assert.equal(nextStep("applied"), "verify");
  for (const status of ["invalid", "applying", "verified", "verificationFailed", "failed"]) {
    assert.equal(nextStep(status), null, status);
  }
});

test("only the per-change checks are gates; alerts and graph nodes are observations", () => {
  const result = verification(
    op("verified", {
      checks: [
        { kind: "addOutput", outputId: "o9", passed: true, reason: "stopped" },
        { kind: "stopOutput", outputId: null, passed: false, reason: "stillRunning" },
      ],
      incidentDelta: { delta: 2 },
      graphConvergence: { activeNodes: 5 },
    }),
  );
  assert.deepEqual(result.gates, [
    { label: "addOutput o9", passed: true, detail: "stopped" },
    { label: "stopOutput", passed: false, detail: "stillRunning" },
  ]);
  assert.deepEqual(result.observations, [
    "Open alerts: 2 more than before the change.",
    "Active graph nodes after the change: 5.",
  ]);
  assert.match(verification(op("verified", { incidentDelta: { delta: -1 } })).observations[0], /1 fewer/);
  assert.match(verification(op("verified", { incidentDelta: { delta: 0 } })).observations[0], /as many as/);
  assert.deepEqual(verification(op("applied")), { gates: [], observations: [] });
});
