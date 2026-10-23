import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { absoluteTime, duration, fullTime, relativeTime, zoneName } = await loadCompiledFrontendModule("control-room/when.js");

const MIN = 60_000;
const HOUR = 60 * MIN;
// Fri 23 Oct 2026, 15:32:18 in London (BST, UTC+1).
const NOW = Date.UTC(2026, 9, 23, 14, 32, 18);
const LONDON = { timeZone: "Europe/London" };

test("relative times read as people say them, both ways", () => {
  assert.equal(relativeTime(NOW - 10_000, NOW), "just now");
  assert.equal(relativeTime(NOW - 3 * MIN, NOW), "3 min ago");
  assert.equal(relativeTime(NOW + 2 * HOUR, NOW), "in 2 h");
  assert.equal(relativeTime(NOW - 30 * HOUR, NOW), "30 h ago");
  assert.equal(relativeTime(NOW - 40 * HOUR, NOW), "yesterday");
  assert.equal(relativeTime(NOW - 4 * 24 * HOUR, NOW), "4 d ago");
});

test("clock times carry as much date as the distance needs, with the zone", () => {
  assert.equal(absoluteTime(NOW, NOW, LONDON), "Today 15:32 BST");
  assert.equal(absoluteTime(NOW, NOW, { ...LONDON, seconds: true, noZone: true }), "Today 15:32:18");
  // 23:30 UTC on the 22nd is 00:30 BST on the 23rd: still today in London.
  assert.equal(absoluteTime(Date.UTC(2026, 9, 22, 23, 30), NOW, LONDON), "Today 00:30 BST");
  assert.equal(absoluteTime(NOW - 20 * HOUR, NOW, LONDON), "Yesterday 19:32 BST");
  assert.equal(absoluteTime(NOW + 26 * HOUR, NOW, LONDON), "Tomorrow 17:32 BST");
  assert.equal(absoluteTime(NOW - 4 * 24 * HOUR, NOW, LONDON), "Mon 15:32 BST");
  // After the clocks go back (25 Oct), London is on GMT.
  assert.equal(absoluteTime(NOW + 10 * 24 * HOUR, NOW, LONDON), "2 Nov, 14:32 GMT");
  assert.equal(absoluteTime(Date.UTC(2025, 2, 1, 9, 0), NOW, LONDON), "1 Mar 2025, 09:00 GMT");
});

test("a tooltip gives the full instant locally and in UTC", () => {
  assert.equal(fullTime(NOW, "Europe/London"), "Fri, 23 Oct 2026, 15:32:18 BST (14:32:18 UTC)");
  assert.equal(zoneName(NOW, "Europe/London"), "BST");
});

test("durations drop units that do not matter", () => {
  assert.equal(duration(45_000), "45 s");
  assert.equal(duration(12 * MIN), "12 min");
  assert.equal(duration(83 * MIN), "1 h 23 min");
  assert.equal(duration(2 * HOUR), "2 h");
  assert.equal(duration(52 * HOUR), "2 d 4 h");
});

test("a retry says which attempt and when the next one comes", async () => {
  const { countdown, retryWords } = await loadCompiledFrontendModule("control-room/when.js");
  assert.equal(countdown(45_000), "45 s");
  assert.equal(countdown(99_000), "1 min 39 s");
  assert.equal(countdown(120_000), "2 min");
  assert.equal(retryWords(7, 99_000), "Attempt 7 · next try in 1 min 39 s");
  assert.equal(retryWords(3, 0), "Attempt 3 · trying again now");
});
