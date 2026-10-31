import assert from "node:assert/strict";
import test from "node:test";

import { axisLabels } from "../../web/ts/ui/series-geometry.ts";

test("an axis never prints the same tick twice", () => {
  const bytes = (value) => `${Math.round(value)} b`;
  // An empty rate chart: the scale tops out at 1, so the middle ticks would round to the ends.
  assert.deepEqual(axisLabels(1, [0, 0.25, 0.5, 0.75, 1], bytes), ["0 b", "", "", "", "1 b"]);
  assert.deepEqual(axisLabels(100, [0, 0.5, 1], bytes), ["0 b", "50 b", "100 b"]);
});

test("a count agrees with its noun", async () => {
  const { counted } = await import("../../web/ts/control-room/format.ts");
  assert.equal(counted(1, "destination"), "1 destination");
  assert.equal(counted(0, "destination"), "0 destinations");
  assert.equal(counted(1200, "sample"), "1,200 samples");
  assert.equal(counted(2, "repeat", "repeats"), "2 repeats");
});
