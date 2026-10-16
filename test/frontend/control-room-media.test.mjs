import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { mediaKey, recordingTimeline, sourceDraft } = await loadCompiledFrontendModule(
  "control-room/media-model.js",
);

const START = "2026-10-15T13:00:00Z";
const END = "2026-10-15T13:42:00Z";
const REMUX = "2026-10-15T13:43:00Z";

function recording(overrides = {}) {
  return {
    name: "recording_20261015T130000_main_ab12.ts",
    size: 1,
    modifiedAt: END,
    recordingId: "rec_ab12",
    pipelineId: "p",
    recordingStatus: "ready",
    recordingStartedAt: START,
    recordingEndedAt: END,
    recordingCodecSummary: "h264 + aac",
    ...overrides,
  };
}

const steps = (events) => events.map((e) => [e.atMs, e.label, e.tone]);

test("a finished recording reads start, stop, then the browser remux", () => {
  assert.deepEqual(
    steps(recordingTimeline(recording({ conversionStatus: "ready", conversionUpdatedAt: REMUX }))),
    [
      [Date.parse(START), "Recording started · h264 + aac", "ok"],
      [Date.parse(END), "Recording stopped and finalized", "ok"],
      [Date.parse(REMUX), "Remuxed to browser MP4", "ok"],
    ],
  );
});

test("a live, finalizing or failed recording says so; an unknown time is a gap", () => {
  assert.deepEqual(
    steps(recordingTimeline(recording({ recordingStatus: "recording", recordingEndedAt: null }))).slice(1),
    [[null, "Still recording", "info"]],
  );
  assert.deepEqual(
    steps(recordingTimeline(recording({ recordingStatus: "finalizing", recordingEndedAt: null }))).slice(1),
    [[null, "Stopped; finalizing the file", "info"]],
  );
  assert.deepEqual(
    steps(
      recordingTimeline(
        recording({
          recordingStatus: "failed",
          recordingError: "disk full",
          conversionStatus: "failed",
          conversionError: "no mov muxer",
          conversionUpdatedAt: "not a time",
        }),
      ),
    ).slice(1),
    [
      [Date.parse(END), "Recording failed: disk full", "error"],
      [null, "Remux failed: no mov muxer", "error"],
    ],
  );
});

test("a file no recording session wrote has no timeline", () => {
  assert.equal(recordingTimeline({ name: "clip.mp4", size: 1, modifiedAt: END }), null);
  // A session row without a start time is not one the page can place.
  assert.equal(recordingTimeline(recording({ recordingStartedAt: null })), null);
});

test("use as source: a looping file pipeline, Live Optimized only when the GOP needs it", () => {
  const file = { name: "intro.final.mp4", size: 1, modifiedAt: END };
  assert.deepEqual(sourceDraft(file, { kind: "ready", detail: "" }), {
    name: "intro.final",
    source: "file",
    filename: "intro.final.mp4",
    loop: true,
    liveOptimized: false,
  });
  assert.equal(sourceDraft(file, { kind: "needsReencode", detail: "" }).liveOptimized, true);
  // The probe failed: the operator decides; nothing is turned on blind.
  assert.equal(sourceDraft(file, null).liveOptimized, false);
  assert.equal(sourceDraft(file, { kind: "noVideo", detail: "" }), null);
});

test("a recording stays selected when its .ts becomes the finalized .mp4", () => {
  const writing = recording({ recordingStatus: "recording" });
  const finalized = recording({ name: "recording_20261015T130000_main_ab12.mp4" });
  assert.equal(mediaKey(writing), mediaKey(finalized));
  // Two sources with the same session-less name are the same row; different
  // names are different rows.
  assert.equal(mediaKey({ name: "a.mp4" }), "a.mp4");
  assert.notEqual(mediaKey({ name: "a.mp4" }), mediaKey({ name: "b.mp4" }));
});
