import assert from "node:assert/strict";
import test from "node:test";

import { startPath } from "../../web/ts/control-room/start-here-model.ts";

function row(id, overrides = {}) {
  return { id, name: id, source: null, protocols: new Map(), delivering: 0, ...overrides };
}

const done = (path) => path.steps.filter((step) => step.done).map((step) => step.key);

test("an empty plant starts at creating a pipeline", () => {
  const path = startPath([]);
  assert.equal(path.next, "pipeline");
  assert.equal(path.pipelineId, null);
  assert.deepEqual(done(path), []);
});

test("each step is read from the plant, on the pipeline furthest along", () => {
  const fresh = row("fresh");
  const keyed = row("keyed", { source: { protocol: "rtmp", videoCodec: "h264", bonded: false } });
  let path = startPath([fresh, keyed]);
  assert.equal(path.pipelineId, "keyed", "the one with a source");
  assert.equal(path.next, "destination");
  assert.deepEqual(done(path), ["pipeline", "encoder"]);

  // A destination added before the encoder connects: that step is done,
  // the encoder is still next.
  const routed = row("routed", { protocols: new Map([["rtmp", 1]]) });
  path = startPath([fresh, routed]);
  assert.equal(path.pipelineId, "routed");
  assert.equal(path.next, "encoder");
  assert.deepEqual(done(path), ["pipeline", "destination"]);

  const live = row("live", { source: keyed.source, protocols: new Map([["rtmp", 2]]), delivering: 1 });
  path = startPath([fresh, keyed, live]);
  assert.equal(path.pipelineId, "live");
  assert.equal(path.next, null, "nothing left: the list leaves Fleet");
  assert.deepEqual(done(path), ["pipeline", "encoder", "destination", "live"]);
});
