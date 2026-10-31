import assert from "node:assert/strict";
import test from "node:test";

import {
  FLOOR_DB,
  channelLabels,
  declaredChannels,
  levelDb,
  meterFraction,
} from "../../web/ts/control-room/audio-level.ts";

test("a full-scale square wave is 0 dBFS, half scale about -6, silence the floor", () => {
  const square = Float32Array.from({ length: 512 }, (_, index) => (index % 2 ? 1 : -1));
  assert.equal(levelDb(square), 0);
  assert.ok(Math.abs(levelDb(square.map((value) => value / 2)) - -6.02) < 0.01);
  assert.equal(levelDb(new Float32Array(512)), FLOOR_DB);
  assert.equal(levelDb(new Float32Array(512).fill(1e-6)), FLOOR_DB, "below the floor reads as the floor");
});

test("the meter maps the floor to empty and full scale to full", () => {
  assert.equal(meterFraction(FLOOR_DB), 0);
  assert.equal(meterFraction(0), 1);
  assert.equal(meterFraction(-30), 0.5);
  assert.equal(meterFraction(6), 1);
});

test("channels are named in Web Audio order, others numbered", () => {
  assert.deepEqual(channelLabels(1), ["M"]);
  assert.deepEqual(channelLabels(2), ["L", "R"]);
  assert.deepEqual(channelLabels(6), ["L", "R", "C", "LFE", "Ls", "Rs"]);
  assert.deepEqual(channelLabels(3), ["1", "2", "3"]);
  assert.deepEqual(channelLabels(0), ["M"], "at least one channel");
});

test("the channels an HLS rendition declares, clamped, stereo when unsaid", () => {
  assert.equal(declaredChannels("1"), 1);
  assert.equal(declaredChannels("6"), 6);
  assert.equal(declaredChannels("16/JOC"), 8, "clamped to what the meter draws");
  assert.equal(declaredChannels(undefined), 2);
  assert.equal(declaredChannels("x"), 2);
  assert.equal(declaredChannels("0"), 2);
});
