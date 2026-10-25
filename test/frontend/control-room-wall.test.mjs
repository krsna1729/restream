import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { MAX_WATCHED_TILES, buildWall, prunedWatched, toggleWatched, wallColumns, watchAll } =
  await loadCompiledFrontendModule("control-room/wall-model.js");

function row(id, name, overrides = {}) {
  return {
    id,
    name,
    source: { protocol: "srt", videoCodec: "h264", bonded: false },
    standbyInputs: 0,
    recording: false,
    ingestBps: 4_000_000,
    egressBps: null,
    wanted: 2,
    delivering: 2,
    attention: 0,
    pending: 0,
    protocols: new Map(),
    ...overrides,
  };
}

function model(rows) {
  return { pipelines: rows, attention: [], pipelinesTotal: rows.length };
}

function alert(pipelineId, title, severity) {
  return { id: `${pipelineId}-${title}`, severity, scope: "pipeline", pipelineId, title };
}

const rows = [
  row("p-b", "Bravo"),
  row("p-a", "Alpha", { attention: 1, delivering: 1 }),
  row("p-c", "Charlie", { source: null, ingestBps: null, delivering: 0 }),
  row("p-d", "Delta"),
];
const alerts = [
  alert("p-d", "Picture is black", "critical"),
  alert("p-d", "Audio track 2 is silent", "warning"),
  alert(undefined, "Host CPU high", "warning"),
];

test("tiles keep fixed places by name and carry their tally, source and alarms", () => {
  const tiles = buildWall(model(rows), alerts, { sort: "name", filter: "all" });
  assert.deepEqual(tiles.map((tile) => tile.name), ["Alpha", "Bravo", "Charlie", "Delta"]);
  const byName = Object.fromEntries(tiles.map((tile) => [tile.name, tile]));
  assert.equal(byName.Alpha.tally, "warning", "a degraded destination is attention");
  assert.equal(byName.Bravo.tally, "live");
  assert.equal(byName.Charlie.tally, "idle");
  assert.equal(byName.Charlie.source, null);
  assert.equal(byName.Delta.tally, "alarm", "a critical alarm outranks everything");
  assert.deepEqual(byName.Delta.alarms.map((a) => a.title), ["Picture is black", "Audio track 2 is silent"]);
  assert.equal(byName.Bravo.source, "srt · h264");
  assert.equal(byName.Bravo.alarms.length, 0, "an alert without a pipeline lands on no tile");
});

test("worst first and the filters", () => {
  const worst = buildWall(model(rows), alerts, { sort: "worst", filter: "all" });
  assert.deepEqual(worst.map((tile) => tile.name), ["Delta", "Alpha", "Bravo", "Charlie"]);
  const live = buildWall(model(rows), alerts, { sort: "name", filter: "live" });
  assert.deepEqual(live.map((tile) => tile.name), ["Alpha", "Bravo", "Delta"]);
  const attention = buildWall(model(rows), alerts, { sort: "name", filter: "alarms" });
  assert.deepEqual(attention.map((tile) => tile.name), ["Alpha", "Delta"]);
});

test("auto layout fits every tile in the smallest square, at most six across", () => {
  assert.equal(wallColumns("auto", 0), 1);
  assert.equal(wallColumns("auto", 1), 1);
  assert.equal(wallColumns("auto", 4), 2);
  assert.equal(wallColumns("auto", 5), 3);
  assert.equal(wallColumns("auto", 16), 4);
  assert.equal(wallColumns("auto", 200), 6);
  assert.equal(wallColumns(3, 200), 3, "a chosen layout is kept");
});

test("watching is capped and only live tiles play", () => {
  const many = Array.from({ length: 12 }, (_, i) => row(`p${i}`, `P${String(i).padStart(2, "0")}`));
  const tiles = buildWall(model([...many, rows[2]]), [], { sort: "name", filter: "all" });
  const all = watchAll(tiles);
  assert.equal(all.length, MAX_WATCHED_TILES);
  assert.ok(!all.includes("p-c"), "an idle tile never plays");

  let watched = [];
  for (const tile of tiles) watched = toggleWatched(watched, tile.id);
  assert.equal(watched.length, MAX_WATCHED_TILES, "starting past the cap is refused");
  assert.deepEqual(toggleWatched(watched, watched[0]).length, MAX_WATCHED_TILES - 1, "stop frees a slot");

  const gone = buildWall(model(many.slice(1)), [], { sort: "name", filter: "all" });
  assert.ok(!prunedWatched(["p0", "p1"], gone).includes("p0"), "a deleted pipeline stops playing");
  assert.deepEqual(prunedWatched(["p1"], gone), ["p1"]);
});
