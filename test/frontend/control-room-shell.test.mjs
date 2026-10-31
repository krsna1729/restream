import assert from "node:assert/strict";
import test from "node:test";

import { paletteEntries, rank } from "../../web/ts/control-room/shell/palette-model.ts";
import { compactCount, navBadges } from "../../web/ts/control-room/shell/badges.ts";
import { resolveTheme } from "../../web/ts/ui/theme.ts";

function output(id, name, overrides = {}) {
  return {
    id,
    pipelineId: "p1",
    name,
    protocol: "rtmp",
    shard: null,
    stage: null,
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

const STATE = {
  epoch: "e",
  revision: 1,
  pipelines: [
    { id: "p1", name: "Temple Main", ingest: null, standbyInputs: 0, recording: false },
    { id: "p2", name: "Studio B", ingest: null, standbyInputs: 0, recording: false },
  ],
  outputs: [
    output("o1", "YouTube main"),
    output("o2", "Twitch backup", {
      diagnosis: { layer: "destination", cause: { code: "noProgress" }, ruledOut: [] },
    }),
  ],
  inputs: [],
  graphs: [],
};

test("the palette finds pipelines, destinations and incidents and opens them where they live", () => {
  const entries = paletteEntries(STATE, [{ id: "a1", title: "Twitch backup stalled", severity: "critical", cause: "no progress", pipelineId: "p1" }], null);
  const titles = (query) => rank(entries, query).map((entry) => `${entry.kind}:${entry.label}`);
  // A label that starts with the query outranks one that only contains it.
  assert.deepEqual(titles("tw").slice(0, 2), ["incident:Twitch backup stalled", "destination:Twitch backup"]);
  // Every term must match; detail text (the pipeline name) counts.
  assert.deepEqual(titles("youtube temple"), ["destination:YouTube main"]);
  assert.deepEqual(titles("nothing here"), []);
  const destination = rank(entries, "youtube")[0];
  assert.deepEqual(destination.place, { workspace: "operate", pipelineId: "p1" });
  // With no query: workspaces and incidents, never ten thousand destinations.
  assert.ok(rank(entries, "").every((entry) => entry.kind === "workspace" || entry.kind === "incident"));
});

test("nav badges count what needs eyes and never invent a zero", () => {
  assert.deepEqual(navBadges(null, null), {});
  const badges = navBadges(STATE, { total: 3, critical: 1 });
  assert.equal(badges.fleet.count, 1);
  assert.equal(badges.fleet.tone, "warn");
  assert.equal(badges.incidents.tone, "bad");
  assert.equal(badges.pipelines.count, 2);
  // No open alert: no badge at all, not a "0".
  assert.equal(navBadges(STATE, { total: 0, critical: 0 }).incidents, undefined);
  assert.equal(compactCount(999), "999");
  assert.equal(compactCount(1234), "1.2k");
  assert.equal(compactCount(10_400), "10k");
});

test("the theme follows the system only when asked to", () => {
  assert.equal(resolveTheme("system", true), "restream-dark");
  assert.equal(resolveTheme("system", false), "restream-light");
  assert.equal(resolveTheme("light", true), "restream-light");
  assert.equal(resolveTheme("dark", false), "restream-dark");
});
