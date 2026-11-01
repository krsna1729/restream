import assert from "node:assert/strict";
import test from "node:test";

import { editDistance, paletteEntries, rank } from "../../web/ts/control-room/shell/palette-model.ts";
import { changeRequest, draftFor } from "../../web/ts/control-room/changes-model.ts";
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
  // Every term must match; detail text (the pipeline name) counts. Without a
  // verb the query names a place, so no command is offered.
  assert.deepEqual(titles("youtube temple"), ["destination:YouTube main"]);
  assert.deepEqual(titles("nothing here"), []);
  const destination = rank(entries, "youtube")[0];
  assert.deepEqual(destination.place, { workspace: "operate", pipelineId: "p1" });
  // With no query: workspaces and incidents, never ten thousand destinations.
  assert.ok(rank(entries, "").every((entry) => entry.kind === "workspace" || entry.kind === "incident"));
});

test("a loosely worded request reaches the command the Changes form would draft, and only drafts it", () => {
  const entries = paletteEntries(STATE, [], null);
  const top = (query) => rank(entries, query)[0];
  const stopYoutube = { workspace: "changes", pipelineId: "p1", change: { kind: "stopOutput", outputId: "o1" } };
  // Misspelt, reordered, with a synonym or filler words: the same command.
  for (const query of ["stop youtube main", "youtube main stop", "stpo youtbe", "halt the youtube", "please pause youtube"]) {
    assert.equal(top(query)?.kind, "command", query);
    assert.deepEqual(top(query).place, stopYoutube, query);
  }
  // A running destination is offered stop and remove, never start.
  assert.ok(!rank(entries, "start youtube").some((entry) => entry.id === "c:startOutput:o1"));
  // The draft is the request the form makes for "Stop a destination" and YouTube main.
  assert.deepEqual(changeRequest("Stop it", "p1", [draftFor(top("stpo youtbe").place.change)]), {
    intent: "Stop it",
    pipelineId: "p1",
    proposedChanges: [{ kind: "stopOutput", outputId: "o1" }],
  });
  // Nothing acts from the palette: a command opens Changes, where it is
  // judged, created as an operation and approved by role.
  const commands = entries.filter((entry) => entry.kind === "command");
  assert.ok(commands.length > 0);
  assert.ok(commands.every((entry) => entry.action === undefined && entry.place?.workspace === "changes"));
  // A role that cannot open Changes is offered no command.
  assert.ok(!paletteEntries(STATE, [], null, (workspace) => workspace !== "changes").some((entry) => entry.kind === "command"));
});

test("a typo is forgiven per word, but short terms and far words must match", () => {
  assert.equal(editDistance("stpo", "stop", 1), 1, "a swap of neighbours is one edit");
  assert.equal(editDistance("youtbe", "youtube", 1), 1);
  assert.equal(editDistance("twitch", "youtube", 1), 2, "gives up once past the budget");
  const entries = paletteEntries(STATE, [], null);
  assert.equal(rank(entries, "twitvh backup")[0]?.label, "Twitch backup");
  // Three letters or fewer are never guessed at ("man" is not "main", which
  // four letters reach); every term must still match.
  assert.deepEqual(rank(entries, "man"), []);
  assert.ok(rank(entries, "mian").some((entry) => entry.label === "YouTube main"));
  assert.deepEqual(rank(entries, "zzzz youtube"), []);
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
