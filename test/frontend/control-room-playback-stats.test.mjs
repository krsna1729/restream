import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { MAX_PLAYBACK_SAMPLES, appendSample, playbackReport, summarizePlayback } = await loadCompiledFrontendModule(
  "control-room/playback-stats.js",
);

function sample(atMs, overrides = {}) {
  return {
    atMs,
    state: "playing",
    width: 1920,
    height: 1080,
    decodedFrames: (atMs / 1000) * 30,
    droppedFrames: 0,
    bufferAheadSec: 2,
    latencySec: 3.5,
    bandwidthBps: 8_000_000,
    ...overrides,
  };
}

test("a summary reads the last readings: size, frame rate, buffer, latency", () => {
  const summary = summarizePlayback([sample(1000), sample(2000), sample(3000)]);
  assert.equal(summary.resolution, "1920×1080");
  assert.equal(summary.fps, 30);
  assert.equal(summary.droppedShare, 0);
  assert.equal(summary.bufferAheadSec, 2);
  assert.equal(summary.latencySec, 3.5);
  assert.equal(summary.bandwidthBps, 8_000_000);
  assert.equal(summary.stalls, 0);
});

test("dropped frames are a share of all frames since the start", () => {
  const summary = summarizePlayback([
    sample(1000, { decodedFrames: 0, droppedFrames: 0 }),
    sample(2000, { decodedFrames: 90, droppedFrames: 10 }),
  ]);
  assert.equal(summary.droppedShare, 0.1);
});

test("stalls count each fall into buffering after playback began", () => {
  const states = ["buffering", "playing", "buffering", "buffering", "playing", "buffering", "playing"];
  const summary = summarizePlayback(states.map((state, index) => sample((index + 1) * 1000, { state })));
  assert.equal(summary.stalls, 2, "the start-up wait is not a stall; a long stall counts once");
});

test("nothing reported reads as unknown, never zero", () => {
  const summary = summarizePlayback([sample(1000, { decodedFrames: null, droppedFrames: null, latencySec: null, width: 0, height: 0 })]);
  assert.equal(summary.resolution, null);
  assert.equal(summary.fps, null);
  assert.equal(summary.droppedShare, null);
  assert.equal(summary.latencySec, null);
  assert.deepEqual(summarizePlayback([]).resolution, null);
  const reset = summarizePlayback([sample(5000, { decodedFrames: 500 }), sample(6000, { decodedFrames: 10 })]);
  assert.equal(reset.fps, null, "a counter that went backwards (a new element) says nothing");
});

test("readings are kept for ten minutes", () => {
  let samples = [];
  for (let index = 0; index < MAX_PLAYBACK_SAMPLES + 5; index += 1) samples = appendSample(samples, sample(index * 1000));
  assert.equal(samples.length, MAX_PLAYBACK_SAMPLES);
  assert.equal(samples[0].atMs, 5000, "the oldest go first");
});

test("the export carries what played, the summary, every reading and event", () => {
  const samples = [sample(1000), sample(2000)];
  const events = [{ atMs: 500, kind: "starting" }];
  const report = playbackReport({ label: "program", url: "/hls/p/master.m3u8", userAgent: "UA", exportedAtMs: 0 }, samples, events);
  assert.equal(report.format, "restream.playback.v1");
  assert.equal(report.exportedAt, "1970-01-01T00:00:00.000Z");
  assert.deepEqual(report.samples, samples);
  assert.deepEqual(report.events, events);
  assert.equal(report.summary.fps, 30);
  assert.doesNotThrow(() => JSON.parse(JSON.stringify(report)));
});
