import assert from "node:assert/strict";
import test from "node:test";

import { ONBOARDING, PAGE_HELP, moveStep, stepsFor } from "../../web/ts/control-room/help-model.ts";
import { GUIDES, canFollow } from "../../web/ts/control-room/guides-model.ts";

test("a tour shows a role only what it may act on and open", () => {
  const steps = [
    { title: "anyone" },
    { title: "operators", role: "operator" },
    { title: "team page", workspace: "team" },
  ];
  assert.deepEqual(stepsFor(steps, "viewer").map((step) => step.title), ["anyone"]);
  assert.deepEqual(stepsFor(steps, "operator").map((step) => step.title), ["anyone", "operators"]);
  assert.deepEqual(stepsFor(steps, "admin").map((step) => step.title), ["anyone", "operators", "team page"]);
});

test("Next past the last step ends a tour; Back stops at the first", () => {
  assert.equal(moveStep(0, 1, 3), 1);
  assert.equal(moveStep(2, 1, 3), null);
  assert.equal(moveStep(0, -1, 3), 0);
});

test("every role gets an onboarding tour and every page a tour step", () => {
  for (const role of ["viewer", "operator", "admin"]) {
    assert.ok(stepsFor(ONBOARDING, role).length >= 5, role);
    for (const [workspace, help] of Object.entries(PAGE_HELP)) {
      assert.ok(stepsFor(help.tour, role).length > 0, `${role} ${workspace}`);
    }
  }
});

test("a guide is followable from its role up", () => {
  const share = GUIDES.find((guide) => guide.id === "share");
  assert.equal(canFollow("operator", share), false);
  assert.equal(canFollow("admin", share), true);
});
