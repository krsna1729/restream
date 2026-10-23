import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { bitrateWords, overallSeverity, streamVerdicts } = await loadCompiledFrontendModule(
  "control-room/stream-health-model.js",
);

const OUTPUTS = {
  wanted: 0,
  delivering: 0,
  degraded: 0,
  retrying: 0,
  stalled: 0,
  retryAttempts: 0,
  offeredBps: 0,
  deliveredBps: 0,
  resyncs: 0,
  worstFeedLag: 0,
};

function sample(atMs, ingest) {
  return {
    atMs,
    ring: null,
    outputs: OUTPUTS,
    ingest:
      ingest === null
        ? null
        : {
            inputId: "in",
            protocol: "srt",
            bitrateBps: 6_000_000,
            packetsPerSec: 500,
            videoCodec: "h264",
            width: 1920,
            height: 1080,
            fps: 30,
            gopSec: 2,
            maxGopSec: 2,
            rttMs: 40,
            lossPerSec: 0,
            retransPerSec: 0,
            receiveBuffer: 0.1,
            ...ingest,
          },
  };
}

const severityOf = (verdicts, key) => verdicts.find((verdict) => verdict.key === key)?.severity;

test("a clean publisher is healthy on every count", () => {
  const verdicts = streamVerdicts(Array.from({ length: 10 }, (_, index) => sample(index, {})));
  assert.equal(overallSeverity(verdicts), "ok");
  assert.deepEqual(verdicts.map((verdict) => verdict.key).sort(), ["bitrate", "fps", "keyframes", "loss"]);
});

test("keyframe spacing is judged against the platforms' 2 s ask and 4 s limit", () => {
  const at = (maxGopSec, gopSec = maxGopSec) => streamVerdicts([sample(0, { gopSec, maxGopSec })]);
  assert.equal(severityOf(at(2), "keyframes"), "ok");
  assert.equal(severityOf(at(3), "keyframes"), "warn");
  assert.equal(severityOf(at(6, 4), "keyframes"), "bad");
  assert.match(at(6, 4)[0].title, /6\.0 s apart/);
  assert.equal(severityOf(at(null, null), "keyframes"), "warn", "unknown is not ok");
});

test("bitrate: a collapse is bad, a swing is a warning, and the first sample is not judged", () => {
  const steady = Array.from({ length: 9 }, (_, index) => sample(index, {}));
  assert.equal(severityOf(streamVerdicts([...steady, sample(9, { bitrateBps: 2_000_000 })]), "bitrate"), "bad");
  assert.equal(severityOf(streamVerdicts([...steady, sample(9, { bitrateBps: 8_500_000 })]), "bitrate"), "warn");
  assert.equal(severityOf(streamVerdicts([sample(0, { bitrateBps: null })]), "bitrate"), undefined);
});

test("frame drops, loss, a long link and a full buffer each have a verdict; no source is the only one", () => {
  const base = Array.from({ length: 9 }, (_, index) => sample(index, {}));
  assert.equal(severityOf(streamVerdicts([...base, sample(9, { fps: 18 })]), "fps"), "warn");
  assert.equal(severityOf(streamVerdicts([sample(0, { lossPerSec: 20 })]), "loss"), "bad");
  assert.equal(severityOf(streamVerdicts([sample(0, { rttMs: 400 })]), "rtt"), "warn");
  assert.equal(severityOf(streamVerdicts([sample(0, { receiveBuffer: 0.95 })]), "buffer"), "bad");
  // A TCP publisher reports no loss: no loss verdict, not a clean one.
  assert.equal(severityOf(streamVerdicts([sample(0, { lossPerSec: null })]), "loss"), undefined);
  const gone = streamVerdicts([...base, sample(9, null)]);
  assert.deepEqual(gone.map((verdict) => [verdict.key, verdict.severity]), [["source", "bad"]]);
  // Worst first.
  const mixed = streamVerdicts([sample(0, { lossPerSec: 20, maxGopSec: 3, gopSec: 3 })]);
  assert.equal(mixed[0].severity, "bad");
  assert.deepEqual(streamVerdicts([]), []);
});

test("only a pushed source has a contribution link to judge", async () => {
  const { contributionTransport } = await loadCompiledFrontendModule("control-room/stream-health-model.js");
  assert.equal(contributionTransport("srt"), "SRT");
  assert.equal(contributionTransport("RTMP"), "TCP");
  assert.equal(contributionTransport("file"), null);
  assert.equal(contributionTransport(undefined), null);
});

test("a bitrate reads against what platforms suggest for its picture", () => {
  assert.deepEqual(bitrateWords(4.5e6, 1080, 30), { severity: "ok", words: "about right for 1080p30" });
  assert.equal(bitrateWords(2e6, 1080, 60).severity, "warn");
  assert.match(bitrateWords(2e6, 1080, 60).words, /low for 1080p60 \(platforms suggest 4.5–9 Mb\/s\)/);
  assert.match(bitrateWords(12e6, 720, 30).words, /more than 720p30 needs/);
  assert.equal(bitrateWords(null, 1080, 30), null);
});
