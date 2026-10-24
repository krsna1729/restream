import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { audioTrackWords, bitrateWords, loudnessWords, overallSeverity, qaCostWords, qaVerdicts, streamVerdicts, trackSoundWords } = await loadCompiledFrontendModule(
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

const qa = (overrides) => ({
  pictureCheckedMs: 100_000,
  blackSinceMs: null,
  stillSinceMs: null,
  soundCheckedMs: 100_000,
  silentSinceMs: null,
  loudnessLufs: -16,
  ...overrides,
});
const qaOf = (verdicts, key) => verdicts.find((verdict) => verdict.key === key);

test("picture and sound: nothing while the checks are off, a moving picture at a good level is ok", () => {
  assert.deepEqual(qaVerdicts(null, 100_000), []);
  const verdicts = qaVerdicts(qa({}), 100_000);
  assert.equal(qaOf(verdicts, "picture").severity, "ok");
  assert.equal(qaOf(verdicts, "sound").severity, "ok");
  assert.match(qaOf(verdicts, "sound").title, /-16 LUFS/);
  // They join the stream's verdicts when the newest sample carries them.
  const keys = streamVerdicts([sample(100_000, { qa: qa({}) })]).map((verdict) => verdict.key);
  assert.ok(keys.includes("picture") && keys.includes("sound"));
});

test("black, still and silence turn amber, then red (or amber for a still) at the engine's alert lines", () => {
  const at = (overrides) => qaVerdicts(qa(overrides), 100_000);
  assert.equal(qaOf(at({ blackSinceMs: 95_000 }), "picture").severity, "warn", "5 s black");
  assert.equal(qaOf(at({ blackSinceMs: 90_000 }), "picture").severity, "bad", "10 s black");
  assert.equal(qaOf(at({ stillSinceMs: 98_000 }), "picture").title, "Picture is moving", "2 s unchanged is a pause");
  assert.equal(qaOf(at({ stillSinceMs: 80_000 }), "picture").severity, "ok", "20 s still: shown, not flagged");
  assert.equal(qaOf(at({ stillSinceMs: 70_000 }), "picture").severity, "warn", "30 s still");
  assert.equal(qaOf(at({ blackSinceMs: 90_000, stillSinceMs: 50_000 }), "picture").title, "Picture black for 10 s", "black wins");
  assert.equal(qaOf(at({ silentSinceMs: 95_000 }), "sound").severity, "warn");
  assert.equal(qaOf(at({ silentSinceMs: 90_000 }), "sound").severity, "bad");
});

test("a still slate is told from a frozen picture, and silent later tracks are named", () => {
  const at = (overrides) => qaVerdicts(qa(overrides), 100_000);
  assert.equal(qaOf(at({ stillSinceMs: 70_000 }), "picture").title, "Picture frozen for 30 s");
  const slate = qaOf(at({ stillSinceMs: 70_000, stillIsSlate: true }), "picture");
  assert.equal(slate.title, "Slate on for 30 s");
  assert.equal(slate.severity, "warn");
  assert.equal(qaOf(at({ tracks: [{ silentSinceMs: null, loudnessLufs: -16 }] }), "tracks"), undefined);
  const silent = qaOf(
    at({
      tracks: [
        { silentSinceMs: null, loudnessLufs: -16 },
        { silentSinceMs: 85_000, loudnessLufs: -70 },
        { silentSinceMs: 98_000, loudnessLufs: -70 },
      ],
    }),
    "tracks",
  );
  assert.equal(silent.title, "Tracks 2, 3 silent");
  assert.equal(silent.severity, "warn", "track 2 has been silent 15 s");
});

test("a stale or missing check says so instead of passing", () => {
  const stale = qaVerdicts(qa({ blackSinceMs: 0 }), 100_000 + 11_000);
  assert.equal(qaOf(stale, "picture").title, "Picture not checked");
  assert.equal(qaOf(stale, "sound").title, "Sound not measured");
  assert.equal(qaOf(qaVerdicts(qa({ soundCheckedMs: null, loudnessLufs: null }), 100_000), "sound").severity, "warn");
});

test("loudness and audio tracks in words", () => {
  assert.equal(loudnessWords(null), null);
  assert.equal(loudnessWords(-14).severity, "ok");
  assert.equal(loudnessWords(-5).severity, "warn");
  assert.equal(loudnessWords(-35).severity, "warn");
  assert.equal(loudnessWords(-65).words, "silent");
  assert.equal(audioTrackWords({ trackIndex: 0, codec: "aac", sampleRate: 48_000, channels: 2 }), "AAC · 48 kHz · stereo");
  assert.equal(audioTrackWords({ trackIndex: 1, codec: "aac", sampleRate: 44_100, channelLayout: "5.1" }), "AAC · 44.1 kHz · 5.1 surround");
  assert.equal(audioTrackWords({ trackIndex: 2, channels: 6 }), "6 channels");
  assert.equal(audioTrackWords({ trackIndex: 3 }), "details not reported yet");
});

test("the checks' cost is stated for the pipeline's tracks, and each track's sound in words", () => {
  assert.equal(qaCostWords(1), "about 2.0% of a CPU core (1 audio track)");
  assert.equal(qaCostWords(16), "about 8.8% of a CPU core (16 audio tracks)");
  assert.equal(qaCostWords(0), "about 2.0% of a CPU core (0 audio tracks)");
  assert.equal(trackSoundWords(undefined), "checking");
  assert.equal(trackSoundWords({ silentSinceMs: 1, loudnessLufs: -70 }), "silent");
  assert.equal(trackSoundWords({ silentSinceMs: null, loudnessLufs: -18.4 }), "-18 LUFS");
});
