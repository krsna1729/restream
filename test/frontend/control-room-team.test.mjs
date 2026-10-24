import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { atLeast, canOpen, viewAsChoices, describeChange, auditOutcome, filterAudit } =
  await loadCompiledFrontendModule("control-room/team-model.js");
const { navGroupsFor } = await loadCompiledFrontendModule("control-room/shell/nav.js");
const { WORKSPACES } = await loadCompiledFrontendModule("control-room/route.js");

test("roles are ordered, and a member views only as a lower role", () => {
  assert.ok(atLeast("admin", "operator") && atLeast("operator", "operator"));
  assert.ok(!atLeast("viewer", "operator"));
  assert.deepEqual(viewAsChoices("admin"), ["viewer", "operator"]);
  assert.deepEqual(viewAsChoices("operator"), ["viewer"]);
  assert.deepEqual(viewAsChoices("viewer"), []);
});

test("navigation offers only what the role may open", () => {
  const items = (role) => navGroupsFor(role).flatMap((group) => group.items);
  assert.ok(items("admin").includes("team") && items("admin").includes("audit"));
  assert.ok(!items("operator").includes("team") && items("operator").includes("settings"));
  assert.ok(!items("viewer").includes("settings"));
  assert.ok(items("viewer").includes("fleet") && items("viewer").includes("incidents"));
  assert.equal(canOpen("viewer", "stability"), true);
});

test("every workspace sits in one group; the deeper tools in the folded one", () => {
  const groups = navGroupsFor("admin");
  const listed = groups.flatMap((group) => group.items);
  assert.deepEqual([...listed].sort(), [...WORKSPACES].sort(), "none dropped, none twice");
  const folded = groups.filter((group) => group.folded).flatMap((group) => group.items);
  assert.ok(folded.includes("stability") && folded.includes("inspect") && folded.includes("capacity"));
  assert.ok(!folded.includes("fleet") && !folded.includes("channels") && !folded.includes("incidents"));
});

test("the audit log reads as plain words, raw when a change is unknown", () => {
  assert.equal(
    describeChange({ method: "POST", path: "/api/v1/pipelines/p1/outputs/o9/start" }),
    "Started destination o9 on p1",
  );
  assert.equal(describeChange({ method: "DELETE", path: "/api/v1/media/a%20b.mp4" }), "Deleted media a b.mp4");
  assert.equal(describeChange({ method: "POST", path: "/api/v1/unknown" }), "POST /api/v1/unknown");
});

test("audit outcomes and filters", () => {
  assert.equal(auditOutcome(201), "done");
  assert.equal(auditOutcome(403), "refused");
  assert.equal(auditOutcome(500), "failed");
  const entries = [
    { id: 2, atMs: 2, username: "ann", role: "viewer", method: "POST", path: "/api/v1/pipelines", status: 403 },
    { id: 1, atMs: 1, username: "bob", role: "operator", method: "POST", path: "/api/v1/pipelines/p/outputs/o/stop", status: 200 },
  ];
  assert.deepEqual(filterAudit(entries, "", "refused").map((e) => e.id), [2]);
  assert.deepEqual(filterAudit(entries, "stopped", "all").map((e) => e.id), [1], "matches the plain words");
  assert.deepEqual(filterAudit(entries, "ANN", "all").map((e) => e.id), [2]);
});
