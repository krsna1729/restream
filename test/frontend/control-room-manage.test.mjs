import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const {
  audioNotes,
  outputDraft,
  outputProblem,
  outputWrite,
  pipelineDraft,
  pipelineProblem,
  pipelineWrite,
} = await loadCompiledFrontendModule("control-room/manage-model.js");

const filePipeline = {
  id: "p1",
  name: "Loop",
  streamKey: "k1",
  inputSource: "file:clip.ts",
  srtIngestPolicy: { mode: "encrypted", passphrase: "a-long-passphrase", pbkeylen: 32, latencyMs: 400 },
  fileIngest: {
    configured: true,
    id: "ingest_1",
    filename: "clip.ts",
    loop: true,
    startTime: "",
    liveOptimized: false,
    targetGopSeconds: 2,
    running: true,
  },
};

test("a new pipeline sends its key only when given, and a live source as null", () => {
  const draft = { ...pipelineDraft(null), name: " Main " };
  assert.deepEqual(pipelineWrite(draft, null), {
    name: "Main",
    inputSource: null,
    fileIngest: null,
    srtIngestPolicy: { mode: "inherit", passphrase: null, pbkeylen: null, latencyMs: null },
  });
  assert.equal(pipelineWrite({ ...draft, streamKey: " my-key " }, null).streamKey, "my-key");
});

test("an edit keeps the key unless rotated, and re-sends the source only when it changed", () => {
  const draft = pipelineDraft(filePipeline);
  const rename = pipelineWrite({ ...draft, name: "Renamed" }, filePipeline);
  // Re-sending an unchanged file source would restart the running ingest.
  assert.equal("inputSource" in rename, false);
  assert.equal("fileIngest" in rename, false);
  assert.equal("streamKey" in rename, false);
  assert.equal(pipelineWrite({ ...draft, rotateKey: true }, filePipeline).streamKey, "");

  const noLoop = pipelineWrite({ ...draft, loop: false }, filePipeline);
  assert.equal(noLoop.fileIngest.loopFlag, false);
  assert.equal(noLoop.inputSource, "file:clip.ts");

  const live = pipelineWrite({ ...draft, source: "live" }, filePipeline);
  assert.equal(live.inputSource, null);
  assert.equal(live.fileIngest, null);
});

test("the SRT override round-trips, and only an encrypted one carries a passphrase", () => {
  const draft = pipelineDraft(filePipeline);
  assert.deepEqual(pipelineWrite(draft, filePipeline).srtIngestPolicy, {
    mode: "encrypted",
    passphrase: "a-long-passphrase",
    pbkeylen: 32,
    latencyMs: 400,
  });
  assert.deepEqual(pipelineWrite({ ...draft, srtMode: "plaintext", srtLatencyMs: "" }, filePipeline).srtIngestPolicy, {
    mode: "plaintext",
    passphrase: null,
    pbkeylen: null,
    latencyMs: null,
  });
});

test("a pipeline the engine would refuse says why", () => {
  const ok = { ...pipelineDraft(null), name: "P" };
  assert.equal(pipelineProblem(ok), null);
  assert.match(pipelineProblem({ ...ok, name: " " }), /name/);
  assert.match(pipelineProblem({ ...ok, source: "file" }), /file/);
  assert.match(pipelineProblem({ ...ok, srtMode: "encrypted", srtPassphrase: "short" }), /10 to 79/);
  assert.match(pipelineProblem({ ...ok, srtLatencyMs: "19" }), /20 to 8000/);
  assert.equal(pipelineProblem({ ...ok, srtLatencyMs: "8000" }), null);
});

const stored = (url, config = { video: { mode: "source" }, audio: { mode: "all" } }) => ({
  id: "o1",
  pipelineId: "p1",
  name: "Out",
  url,
  config,
});

test("a destination's URL survives an edit through presets and SRT fields", () => {
  for (const url of [
    "rtmp://a.rtmp.youtube.com/live2/abcd-efgh",
    "rtmp://cdn.example.com/live/key",
    "srt://dr.example.com:9000?streamid=publish:dr&passphrase=a-long-passphrase&pbkeylen=24&latency=200",
    "https://a.upload.youtube.com/http_upload_hls?cid=abc%20d&copy=0&file=out.m3u8",
  ]) {
    assert.equal(outputWrite(outputDraft(stored(url), "host")).url, url, url);
  }
  const youtube = outputDraft(stored("rtmp://a.rtmp.youtube.com/live2/abcd-efgh"), "host");
  assert.equal(youtube.input, "abcd-efgh");
  // The preset says its RTMP flavour; a custom server uses the chosen one.
  assert.deepEqual(outputWrite(youtube).config.protocol, { type: "rtmp", mode: "enhanced" });
  const custom = outputDraft(stored("rtmp://cdn.example.com/live/key"), "host");
  assert.deepEqual(outputWrite({ ...custom, rtmpMode: "enhanced" }).config.protocol, {
    type: "rtmp",
    mode: "enhanced",
  });
  assert.deepEqual(outputWrite(outputDraft(stored("srt://h:1"), "host")).config.protocol, { type: "auto" });
});

test("video and audio choices become the engine's output config", () => {
  const base = { ...outputDraft(null, "host"), name: "N", input: "rtmp://h/live/k" };
  assert.deepEqual(outputWrite({ ...base, profile: "720p", codec: "h265" }).config.video, {
    mode: "preset",
    preset: "720p",
    codec: "h265",
  });
  assert.deepEqual(outputWrite({ ...base, audio: "selectTracks", tracks: [0, 2] }).config.audio, {
    mode: "selectTracks",
    tracks: [0, 2],
  });
  assert.deepEqual(outputWrite({ ...base, audio: "downmix", tracks: [1] }).config.audio, {
    mode: "downmix",
    track: 1,
  });
  assert.deepEqual(
    outputWrite({ ...base, audio: "remap", tracks: [1], leftChannel: 2, rightChannel: 3 }).config.audio,
    { mode: "remap", track: 1, leftChannel: 2, rightChannel: 3 },
  );
  // A stored remap opens as the same remap.
  const remap = { video: { mode: "source" }, audio: { mode: "remap", track: 1, leftChannel: 2, rightChannel: 3 } };
  assert.deepEqual(outputWrite(outputDraft(stored("rtmp://h/live/k", remap), "host")).config.audio, remap.audio);
});

const stereoOnly = { maxTracks: 1, maxChannels: 2, codecs: ["aac"] };
const anything = { maxTracks: Infinity, maxChannels: Infinity, codecs: "any" };
const tracks = [
  { trackIndex: 0, channels: 2 },
  { trackIndex: 1, channels: 6 },
];

test("a destination the engine would refuse says why, before it is asked", () => {
  const ok = { ...outputDraft(null, "host"), name: "N", input: "rtmp://h/live/k" };
  assert.equal(outputProblem(ok, anything, tracks), null);
  assert.match(outputProblem({ ...ok, name: "" }, anything, tracks), /name/);
  assert.match(outputProblem({ ...ok, input: "" }, anything, tracks), /URL/);
  assert.match(
    outputProblem({ ...ok, protocol: "srt", srt: { ...ok.srt, host: "h", passphrase: "short" } }, anything, tracks),
    /10 to 79/,
  );
  assert.match(outputProblem({ ...ok, audio: "selectTracks", tracks: [0, 1] }, stereoOnly, tracks), /at most 1 audio track/);
  // A 5.1 track passed through to a stereo-only destination must be downmixed.
  assert.match(outputProblem({ ...ok, audio: "all" }, { ...anything, maxChannels: 2 }, tracks), /downmix/);
  assert.equal(outputProblem({ ...ok, audio: "downmix", tracks: [1] }, stereoOnly, tracks), null);
});

test("audio notes say what is dropped or downmixed, and when tracks are unknown", () => {
  const ok = { ...outputDraft(null, "host"), name: "N", input: "rtmp://h/live/k" };
  assert.match(audioNotes(ok, stereoOnly, tracks).join(" "), /other 1 are not sent/);
  assert.match(audioNotes({ ...ok, audio: "downmix", tracks: [1] }, stereoOnly, tracks).join(" "), /downmixed/);
  assert.match(audioNotes(ok, anything, []).join(" "), /unknown/);
  assert.doesNotMatch(audioNotes({ ...ok, audio: "remap" }, stereoOnly, tracks).join(" "), /not sent/);
});
