import assert from "node:assert/strict";
import test from "node:test";

import { encoderFields, goLiveChecks } from "../../web/ts/control-room/go-live-model.ts";

const READY = { sourceOn: true, maxGopSec: 2, destinations: 3, wanted: 3, delivering: 3, utilization: 0.4, safeUtilization: 0.8 };
const states = (facts) => goLiveChecks(facts).map((check) => `${check.key}:${check.state}`);

test("a pipeline on air everywhere passes every check", () => {
  assert.deepEqual(states(READY), ["source:ok", "keyframes:ok", "destinations:ok", "live:ok", "headroom:ok"]);
});

test("the checks point at what to do next, in order", () => {
  // Nothing yet: connect a source and add a destination; no keyframe or
  // live check until there is something to judge.
  assert.deepEqual(states({ ...READY, sourceOn: false, maxGopSec: null, destinations: 0, wanted: 0, delivering: 0 }), [
    "source:todo",
    "destinations:todo",
    "headroom:ok",
  ]);
  assert.deepEqual(states({ ...READY, wanted: 0, delivering: 0 }).slice(3, 4), ["live:todo"]);
  assert.deepEqual(states({ ...READY, delivering: 1 }).slice(3, 4), ["live:warn"]);
  assert.deepEqual(states({ ...READY, utilization: 0.85 }).slice(4), ["headroom:warn"]);
});

test("keyframes: 2 s passes, a little over warns, past 4 s says what to set", () => {
  assert.equal(goLiveChecks({ ...READY, maxGopSec: 2.05 })[1].state, "ok");
  assert.equal(goLiveChecks({ ...READY, maxGopSec: 3 })[1].state, "warn");
  assert.match(goLiveChecks({ ...READY, maxGopSec: 8 })[1].detail, /keyframe interval to 2 s/);
  assert.match(goLiveChecks({ ...READY, maxGopSec: null })[1].title, /not measured/);
});

test("an RTMP URL splits into OBS's server and stream key; SRT goes in whole", () => {
  assert.deepEqual(encoderFields("rtmp://host:1935/live/abc123"), { server: "rtmp://host:1935/live", key: "abc123" });
  assert.deepEqual(encoderFields("srt://host:9000?streamid=publish:abc"), { server: "srt://host:9000?streamid=publish:abc", key: null });
  assert.deepEqual(encoderFields("rtmp://host"), { server: "rtmp://host", key: null });
});
