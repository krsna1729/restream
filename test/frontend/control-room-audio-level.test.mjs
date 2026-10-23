import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { FLOOR_DB, levelDb, meterFraction } = await loadCompiledFrontendModule("control-room/audio-level.js");

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
