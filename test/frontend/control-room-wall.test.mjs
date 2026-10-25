import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { MAX_WATCHED_TILES, buildWall, prunedWatched, toggleWatched, wallColumns, wallScreenFit, watchAll } =
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
  alert("p-d", "Delta: Picture is black", "critical"),
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
  assert.deepEqual(
    byName.Delta.alarms.map((a) => a.title),
    ["Picture is black", "Audio track 2 is silent"],
    "critical first; the tile's own name is not repeated",
  );
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

test("full screen picks the widest tiles that all fit the screen", () => {
  const screen = { width: 1920, height: 1080 };
  const fits = (fit, tiles) => {
    const rows = Math.ceil(tiles / fit.columns);
    return fit.columns * fit.tileWidth <= screen.width + 0.5 && rows * ((fit.tileWidth * 9) / 16 + 64) <= screen.height + 0.5;
  };
  for (const tiles of [1, 2, 3, 4, 5, 9, 12, 16, 30]) {
    const fit = wallScreenFit("auto", tiles, screen);
    assert.ok(fits(fit, tiles), `${tiles} tiles fit without scrolling`);
    for (let columns = 1; columns <= 6; columns += 1) {
      const rows = Math.ceil(tiles / columns);
      const other = Math.min(screen.width / columns, ((screen.height / rows - 64) * 16) / 9);
      assert.ok(other <= fit.tileWidth + 1e-9, `${tiles} tiles: no column count gives wider tiles`);
    }
  }
  assert.equal(wallScreenFit("auto", 1, screen).columns, 1, "one tile fills the height, centred");
  assert.equal(wallScreenFit("auto", 5, screen).columns, 3);
  assert.equal(wallScreenFit("auto", 9, screen).columns, 3);
  assert.deepEqual(wallScreenFit(2, 9, screen), { columns: 2, tileWidth: 960 }, "a chosen layout keeps full-width columns");
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
