import assert from "node:assert/strict";
import test from "node:test";

import * as mediaModel from "../../web/ts/control-room/media-model.ts";

const { mediaKey, recordingTimeline, sourceDraft } = mediaModel;

const START = "2026-10-15T13:00:00Z";
const STOP = "2026-10-15T13:42:00Z";
const WRITTEN = "2026-10-15T13:43:00Z";

function recording(overrides = {}) {
  return {
    name: "recording_20261015T130000_main_ab12.mp4",
    size: 1,
    modifiedAt: WRITTEN,
    recordingId: "rec_ab12",
    pipelineId: "p",
    recordingStatus: "ready",
    recordingStartedAt: START,
    // The recorder stamps endedAt when the MP4 is written, not at stop.
    recordingEndedAt: WRITTEN,
    recordingCodecSummary: "h264 + aac",
    ...overrides,
  };
}

const steps = (events) => events.map((e) => [e.atMs, e.label, e.tone]);

test("a finished recording reads its start, then when its MP4 was written", () => {
  const expected = [
    [Date.parse(START), "Recording started · h264 + aac", "ok"],
    [Date.parse(WRITTEN), "Stopped; MP4 written", "ok"],
  ];
  // The .ts is deleted after the remux, and with it the conversion state...
  assert.deepEqual(steps(recordingTimeline(recording())), expected);
  // ...or kept with a `ready` state: one step either way, never two.
  assert.deepEqual(
    steps(recordingTimeline(recording({ conversionStatus: "ready", conversionUpdatedAt: STOP }))),
    expected,
  );
});

test("while it records, then while the remux runs: a stop time, never 'still recording'", () => {
  const live = recording({ recordingStatus: "recording", recordingEndedAt: null });
  assert.deepEqual(steps(recordingTimeline(live)).slice(1), [[null, "Still recording", "info"]]);
  // At stop the recorder writes only the conversion state; the session row
  // still says recording until the MP4 is written.
  assert.deepEqual(
    steps(
      recordingTimeline({ ...live, conversionStatus: "converting", conversionUpdatedAt: STOP }),
    ).slice(1),
    [[Date.parse(STOP), "Stopped; writing the browser MP4", "info"]],
  );
});

test("a failed remux is one remux failure with the .ts kept; an unknown time is a gap", () => {
  // The recorder marks the session failed with the same FFmpeg error.
  assert.deepEqual(
    steps(
      recordingTimeline(
        recording({
          recordingStatus: "failed",
          recordingEndedAt: null,
          recordingError: "no mov muxer",
          conversionStatus: "failed",
          conversionError: "no mov muxer",
          conversionUpdatedAt: "not a time",
        }),
      ),
    ).slice(1),
    [[null, "MP4 remux failed: no mov muxer; the .ts is kept", "error"]],
  );
  // A failed session with no conversion failure is the recording's own.
  assert.deepEqual(
    steps(
      recordingTimeline(
        recording({ recordingStatus: "failed", recordingEndedAt: null, recordingError: "disk full" }),
      ),
    ).slice(1),
    [[null, "Recording failed: disk full", "error"]],
  );
});

test("a file no recording session wrote has no timeline", () => {
  assert.equal(recordingTimeline({ name: "clip.mp4", size: 1, modifiedAt: WRITTEN }), null);
  // A session row without a start time is not one the page can place.
  assert.equal(recordingTimeline(recording({ recordingStartedAt: null })), null);
});

test("use as source: a looping file pipeline, Live Optimized only when the GOP needs it", () => {
  const file = { name: "intro.final.mp4", size: 1, modifiedAt: WRITTEN };
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
  const writing = recording({ name: "recording_20261015T130000_main_ab12.ts", recordingStatus: "recording" });
  const finalized = recording();
  assert.equal(mediaKey(writing), mediaKey(finalized));
  assert.equal(mediaKey({ name: "a.mp4" }), "a.mp4");
  assert.notEqual(mediaKey({ name: "a.mp4" }), mediaKey({ name: "b.mp4" }));
});

test("a playlist's running time adds its files and says when one is unknown", () => {
  const { playlistRuntime, runtimeWords } = mediaModel;
  assert.deepEqual(playlistRuntime([60, 125.4, 3600]), { totalSec: 3785.4, complete: true });
  assert.equal(runtimeWords(playlistRuntime([60, 125.4, 3600])), "runs 1:03:05");
  assert.equal(runtimeWords(playlistRuntime([60, null])), "runs at least 1:00");
});
