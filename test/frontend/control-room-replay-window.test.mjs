import assert from "node:assert/strict";
import test from "node:test";

import {
  MIN_SPAN_MS,
  clampWindow,
  fromDrag,
  incidentWindow,
  lastSpan,
  pointsFor,
  windowLabel,
  zoom,
} from "../../web/ts/control-room/stability/replay-window.ts";

const MIN = 60_000;
const NOW = Date.UTC(2026, 9, 23, 14, 0);
const DAY_AGO = NOW - 24 * 60 * MIN;

test("a following window zooms out and in with its right edge on now", () => {
  const ten = lastSpan(10 * MIN, NOW);
  assert.deepEqual(zoom(ten, 2, DAY_AGO, NOW), { fromMs: NOW - 20 * MIN, toMs: null });
  assert.deepEqual(zoom(ten, 0.5, DAY_AGO, NOW), { fromMs: NOW - 5 * MIN, toMs: null });
  // Never narrower than two minutes, never older than the history.
  assert.deepEqual(zoom(ten, 0.01, DAY_AGO, NOW), { fromMs: NOW - MIN_SPAN_MS, toMs: null });
  assert.deepEqual(zoom(ten, 1000, DAY_AGO, NOW), { fromMs: DAY_AGO, toMs: null });
});

test("a fixed window zooms around its middle and stays inside the history", () => {
  const hour = { fromMs: NOW - 3 * 60 * MIN, toMs: NOW - 2 * 60 * MIN };
  assert.deepEqual(zoom(hour, 2, DAY_AGO, NOW), { fromMs: NOW - 210 * MIN, toMs: NOW - 90 * MIN });
  assert.deepEqual(zoom(hour, 0.5, DAY_AGO, NOW), { fromMs: NOW - 165 * MIN, toMs: NOW - 135 * MIN });
  // Widened past now, it becomes a following window.
  assert.equal(zoom({ fromMs: NOW - 10 * MIN, toMs: NOW - 5 * MIN }, 8, DAY_AGO, NOW).toMs, null);
});

test("a drag across the overview picks its span; one reaching the end follows", () => {
  const overview = { fromMs: DAY_AGO, toMs: NOW };
  assert.deepEqual(fromDrag(0.75, 0.5, overview, NOW), { fromMs: NOW - 12 * 60 * MIN, toMs: NOW - 6 * 60 * MIN });
  assert.equal(fromDrag(0.9, 1, overview, NOW).toMs, null);
  // A click (no drag) still opens a usable span.
  const click = fromDrag(0.5, 0.5, overview, NOW);
  assert.equal(click.toMs - click.fromMs, MIN_SPAN_MS);
});

test("an incident's window brackets it, and an open one follows now", () => {
  assert.deepEqual(incidentWindow(NOW - 30 * MIN, NOW - 20 * MIN), { fromMs: NOW - 32 * MIN, toMs: NOW - 18 * MIN });
  assert.deepEqual(incidentWindow(NOW - 30 * MIN, null), { fromMs: NOW - 32 * MIN, toMs: null });
  assert.deepEqual(clampWindow({ fromMs: NOW - 2 * 24 * 60 * MIN, toMs: NOW - 47 * 60 * MIN }, DAY_AGO, NOW).fromMs, DAY_AGO);
});

test("a window says its span in words and asks for a bounded number of samples", () => {
  assert.equal(windowLabel(lastSpan(40 * MIN, NOW), NOW, "Europe/London"), "Today 14:20 BST → now · 40 min");
  assert.equal(pointsFor(lastSpan(10 * MIN, NOW), NOW), 300);
  assert.equal(pointsFor(lastSpan(24 * 60 * MIN, NOW), NOW), 1200);
});
