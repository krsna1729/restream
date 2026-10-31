import assert from "node:assert/strict";
import test from "node:test";

import {
  audioTrackWords,
  bitrateWords,
  loudnessWords,
  overallSeverity,
  qaCostWords,
  qaVerdicts,
  streamVerdicts,
  trackMeter,
  loudnessFraction,
  timelineVerdicts,
  decoderVerdicts,
  trackName,
} from "../../web/ts/control-room/stream-health-model.ts";

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
  const { contributionTransport } = await import("../../web/ts/control-room/stream-health-model.ts");
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

test("the checks' cost is stated for the pipeline's tracks", () => {
  assert.equal(qaCostWords(1), "about 2.0% of a CPU core (1 audio track)");
  assert.equal(qaCostWords(16), "about 8.8% of a CPU core (16 audio tracks)");
  assert.equal(qaCostWords(0), "about 2.0% of a CPU core (0 audio tracks)");
});

test("each track's meter: level, how it reads, and nothing from a stale check", () => {
  const now = 100_000;
  assert.equal(trackMeter(undefined, now, now), null, "a track not measured yet has no meter");
  assert.equal(trackMeter({ silentSinceMs: null, loudnessLufs: -18 }, now - 10_001, now), null, "a stale check says nothing about now");
  assert.equal(trackMeter({ silentSinceMs: null, loudnessLufs: -18 }, null, now), null);
  assert.deepEqual(trackMeter({ silentSinceMs: 1, loudnessLufs: -70 }, now, now), { fraction: 0, severity: "bad", words: "silent" });
  const comfortable = trackMeter({ silentSinceMs: null, loudnessLufs: -18.4 }, now, now);
  assert.equal(comfortable.words, "-18 LUFS");
  assert.equal(comfortable.severity, "ok");
  assert.ok(Math.abs(comfortable.fraction - (60 - 18.4) / 60) < 1e-9);
  assert.equal(trackMeter({ silentSinceMs: null, loudnessLufs: -5 }, now, now).severity, "warn", "too loud");
  assert.equal(trackMeter({ silentSinceMs: null, loudnessLufs: -40 }, now, now).severity, "warn", "too quiet");
  assert.equal(loudnessFraction(-60), 0);
  assert.equal(loudnessFraction(-80), 0);
  assert.equal(loudnessFraction(0), 1);
  assert.equal(loudnessFraction(-14), 46 / 60);
});

function inspection(tracks) {
  return { windowSec: 60, unread: 0, tracks };
}

function inspected(media, track, extra = {}) {
  return { media, track, packetsPerSec: media === "video" ? 30 : 47, backwards: 0, jumps: 0, ptsBeforeDts: 0, ...extra };
}

test("timestamps: steady is ok, a reset or a leap is named by track, three are bad", () => {
  assert.deepEqual(timelineVerdicts(null), [], "nothing until the inspector has read the source");
  const steady = timelineVerdicts(inspection([inspected("video", 0), inspected("audio", 0, { avOffsetMs: 40 })]));
  assert.deepEqual(steady.map((verdict) => [verdict.key, verdict.severity]), [["timestamps", "ok"]]);
  const reset = timelineVerdicts(inspection([inspected("video", 0, { backwards: 1 }), inspected("audio", 0)]));
  assert.equal(reset[0].severity, "warn");
  assert.equal(reset[0].title, "Timestamps broke on Video 1");
  assert.match(reset[0].detail, /1 went back/);
  const many = timelineVerdicts(inspection([inspected("video", 0, { jumps: 2 }), inspected("audio", 1, { backwards: 1 })]));
  assert.equal(many[0].severity, "bad");
  assert.equal(many[0].title, "Timestamps broke on Video 1, Audio 2");
  assert.equal(trackName({ media: "audio", track: 0 }), "Audio 1");
});

test("a picture shown before it decodes, audio drift and a large offset each get a verdict", () => {
  const verdicts = timelineVerdicts(
    inspection([
      inspected("video", 0, { ptsBeforeDts: 4 }),
      inspected("audio", 0, { avOffsetMs: 120, clockDriftMsPerMin: 350 }),
      inspected("audio", 1, { avOffsetMs: -1500 }),
      inspected("audio", 2, { avOffsetMs: 300, clockDriftMsPerMin: -1200 }),
    ]),
  );
  const byKey = Object.fromEntries(verdicts.map((verdict) => [verdict.key, verdict]));
  assert.equal(byKey.order.severity, "warn");
  assert.equal(byKey["drift-0"].severity, "warn");
  assert.match(byKey["drift-0"].title, /Audio 1 clock drifts 350 ms a minute/);
  assert.match(byKey["drift-0"].detail, /run fast/);
  assert.equal(byKey["offset-1"].title, "Audio 2 arrives 1.5 s before the video");
  assert.equal(byKey["drift-2"].severity, "bad", "a second a minute is bad");
  assert.equal(byKey["offset-2"], undefined, "drift is said once per track");
  assert.equal(
    timelineVerdicts(inspection([inspected("video", 0), inspected("audio", 0, { avOffsetMs: 300, clockDriftMsPerMin: 150 })])).length,
    1,
    "a steady offset and small drift are fine",
  );
});

test("an FFmpeg stage that cannot keep up is named; one that keeps up says nothing", () => {
  const decoder = (inputBlocked) => ({ stage: "transcode:h264_720p", lagSlots: 40, lagSec: 0.4, inputBlocked });
  assert.deepEqual(decoderVerdicts([decoder(null), decoder(0.05)]), []);
  assert.equal(decoderVerdicts([decoder(0.3)])[0].severity, "warn");
  const bad = decoderVerdicts([decoder(0.6)])[0];
  assert.equal(bad.severity, "bad");
  assert.match(bad.detail, /60% of the time, 0.4 s behind the source/);
});

test("a publisher whose timestamps break and whose encode lags reads as damaged", () => {
  const broken = { timeline: inspection([inspected("video", 0, { backwards: 2, jumps: 1 }), inspected("audio", 0)]) };
  const samples = Array.from({ length: 10 }, (_, index) => sample(index, broken));
  samples[9] = { ...samples[9], decoders: [{ stage: "transcode:h264_720p", lagSlots: 300, lagSec: 3, inputBlocked: 0.7 }] };
  const verdicts = streamVerdicts(samples);
  assert.equal(overallSeverity(verdicts), "bad");
  assert.equal(severityOf(verdicts, "timestamps"), "bad");
  assert.equal(severityOf(verdicts, "decoder-transcode:h264_720p"), "bad");
});

test("captions: on is fine, stopped turns amber then red, none says nothing", () => {
  const on = timelineVerdicts(inspection([inspected("video", 0, { captions: { cea608: true, cea708: true, lastSeenSec: 0, lastTextSec: 2 } })]));
  const byKey = Object.fromEntries(on.map((verdict) => [verdict.key, verdict]));
  assert.equal(byKey["captions-0"].severity, "ok");
  assert.match(byKey["captions-0"].detail, /CEA-608 and CEA-708/);
  const quiet = timelineVerdicts(inspection([inspected("video", 0, { captions: { cea608: true, cea708: false, lastSeenSec: 0, lastTextSec: 300 } })]));
  assert.match(quiet.find((verdict) => verdict.key === "captions-0").detail, /No caption text for 5 min/);
  const stopped = timelineVerdicts(inspection([inspected("video", 0, { captions: { cea608: true, cea708: false, lastSeenSec: 25 } })]));
  const verdict = stopped.find((item) => item.key === "captions-0");
  assert.equal(verdict.severity, "warn");
  assert.equal(verdict.title, "Captions stopped on Video 1 25 s ago");
  const gone = timelineVerdicts(inspection([inspected("video", 0, { captions: { cea608: true, cea708: false, lastSeenSec: 61 } })]));
  assert.equal(gone.find((item) => item.key === "captions-0").severity, "bad");
  assert.equal(
    timelineVerdicts(inspection([inspected("video", 0)])).some((item) => item.key.startsWith("captions")),
    false,
    "a source that never sent captions has no captions verdict",
  );
});

test("frame rate is what arrives (the inspector's count), not what the stream declares", () => {
  const dropping = { fps: 30, timeline: inspection([inspected("video", 0, { packetsPerSec: 13 })]) };
  const verdicts = streamVerdicts(Array.from({ length: 10 }, (_, index) => sample(index, dropping)));
  assert.equal(severityOf(verdicts, "fps"), "warn");
  assert.equal(verdicts.find((verdict) => verdict.key === "fps").title, "13 fps");
  // Without the inspector's reading, the stream's own figure is what there is.
  const declaredOnly = streamVerdicts(Array.from({ length: 10 }, (_, index) => sample(index, { fps: 30 })));
  assert.equal(declaredOnly.find((verdict) => verdict.key === "fps").title, "30 fps");
});
