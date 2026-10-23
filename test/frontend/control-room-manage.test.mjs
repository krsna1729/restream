import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const {
  applyChoice,
  choiceKey,
  destinationChoices,
  moveQueued,
  nowPlayingLabel,
  audioChoiceCounts,
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
  // An SRT access-control stream id keeps its meaning: its `#` must not
  // become a URL fragment when the destination is saved again.
  const access = outputWrite(outputDraft(stored("srt://h:9000?streamid=%23!%3A%3Ar%3Dlive%2Cm%3Dpublish&x=a%26b"), "host"));
  const written = new URL(access.url);
  assert.equal(written.hash, "");
  assert.equal(written.searchParams.get("streamid"), "#!::r=live,m=publish");
  assert.equal(written.searchParams.get("x"), "a&b");
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
  // Custom video settings this editor cannot show are kept, unless replaced.
  const custom = { video: { mode: "custom" }, audio: { mode: "all" } };
  const draft = outputDraft(stored("rtmp://h/live/k", custom), "host");
  assert.equal(draft.profile, null);
  assert.deepEqual(outputWrite({ ...draft, name: "Renamed" }).config.video, custom.video);
  assert.deepEqual(outputWrite({ ...draft, profile: "" }).config.video, { mode: "source", codec: "auto" });
});

const stereoOnly = { maxTracks: 1, maxChannels: 2, codecs: ["aac"] };
const anything = { maxTracks: Infinity, maxChannels: Infinity, codecs: "any" };
const tracks = [
  { trackIndex: 0, channels: 2 },
  { trackIndex: 1, channels: 6 },
];

test("only what the engine cannot take stops a save; the destination's audio limits do not", () => {
  const ok = { ...outputDraft(null, "host"), name: "N", input: "rtmp://h/live/k" };
  assert.equal(outputProblem(ok), null);
  assert.match(outputProblem({ ...ok, name: "" }), /name/);
  assert.match(outputProblem({ ...ok, input: "" }), /URL/);
  assert.match(outputProblem({ ...ok, protocol: "srt", srt: { ...ok.srt, host: "h", passphrase: "short" } }), /10 to 79/);
  assert.match(outputProblem({ ...ok, audio: "selectTracks", tracks: [] }), /at least one/);
  // Too many tracks or channels for the destination: advice, not a refusal.
  assert.equal(outputProblem({ ...ok, audio: "selectTracks", tracks: [0, 1] }), null);
});

test("the track and channel pickers always include what is stored", () => {
  const base = outputDraft(null, "host");
  assert.deepEqual(audioChoiceCounts(base, tracks), { tracks: 2, channels: 2 });
  assert.deepEqual(audioChoiceCounts({ ...base, tracks: [1] }, tracks), { tracks: 2, channels: 6 });
  // Nobody publishes: a stored track 3, channel 5 still show as themselves.
  assert.deepEqual(audioChoiceCounts({ ...base, tracks: [2], leftChannel: 4, rightChannel: 1 }, []), {
    tracks: 3,
    channels: 5,
  });
  assert.deepEqual(audioChoiceCounts({ ...base, tracks: [0, 2] }, []), { tracks: 3, channels: 2 });
});

test("audio notes say what is dropped or downmixed, and when tracks are unknown", () => {
  const ok = { ...outputDraft(null, "host"), name: "N", input: "rtmp://h/live/k" };
  assert.match(audioNotes(ok, stereoOnly, tracks).join(" "), /other 1 are not sent/);
  // "all" to a one-track destination sends the stereo first track: no warning
  // about the 5.1 track it never sends.
  assert.doesNotMatch(audioNotes(ok, stereoOnly, tracks).join(" "), /more than 2 channels/);
  assert.match(audioNotes(ok, { ...anything, maxChannels: 2 }, tracks).join(" "), /more than 2 channels/);
  assert.match(
    audioNotes({ ...ok, audio: "selectTracks", tracks: [0, 1] }, stereoOnly, tracks).join(" "),
    /at most 1 audio track.*2 are selected/,
  );
  assert.match(audioNotes({ ...ok, audio: "downmix", tracks: [1] }, stereoOnly, tracks).join(" "), /downmixed/);
  assert.match(audioNotes(ok, anything, []).join(" "), /unknown/);
  assert.doesNotMatch(audioNotes({ ...ok, audio: "remap" }, stereoOnly, tracks).join(" "), /not sent/);
});

test("a playlist sends every file in order, and one file sends no playlist", () => {
  const draft = { ...pipelineDraft(filePipeline), queue: ["b.mp4", "c.mp4"] };
  const write = pipelineWrite(draft, filePipeline);
  assert.deepEqual(write.fileIngest.playlist, ["clip.ts", "b.mp4", "c.mp4"]);
  assert.equal(write.inputSource, "file:clip.ts");
  assert.equal("playlist" in pipelineWrite({ ...draft, queue: [] }, null).fileIngest, false);

  const stored = { ...filePipeline, fileIngest: { ...filePipeline.fileIngest, playlist: ["clip.ts", "b.mp4"] } };
  assert.deepEqual(pipelineDraft(stored).queue, ["b.mp4"]);
  assert.equal("fileIngest" in pipelineWrite(pipelineDraft(stored), stored), false, "unchanged: not re-sent");
});

test("queued files move within bounds, and a running playlist says what plays", () => {
  assert.deepEqual(moveQueued(["a", "b", "c"], 2, -1), ["a", "c", "b"]);
  assert.deepEqual(moveQueued(["a", "b"], 0, -1), ["a", "b"]);
  assert.equal(nowPlayingLabel(["a", "b", "c"], 1), "2 of 3 · b");
  assert.equal(nowPlayingLabel(["a"], 0), null);
  assert.equal(nowPlayingLabel(["a", "b"], null), null);
});

test("destinations are picked as platforms or custom servers, keeping a stream key between platforms", () => {
  const choices = destinationChoices();
  const by = (label) => choices.find((choice) => choice.label === label);
  assert.deepEqual(choices.slice(-3).map((choice) => choice.label), ["Custom RTMP", "Custom SRT", "Custom HLS"]);
  assert.ok(by("YouTube") && by("Twitch") && by("YouTube (HLS)"));

  const blank = outputDraft(null, "127.0.0.1");
  assert.equal(choiceKey(blank), "rtmp:", "a new destination starts on a custom RTMP server");
  const youtube = applyChoice({ ...blank, input: "rtmp://x/app/key" }, by("YouTube"), by("Custom RTMP"));
  assert.equal(youtube.name, "YouTube", "an empty name takes the platform's");
  assert.equal(youtube.input, "", "a custom URL is not a stream key");
  assert.equal(choiceKey(youtube), by("YouTube").key);

  const twitch = applyChoice({ ...youtube, input: "live_123" }, by("Twitch"), by("YouTube"));
  assert.equal(twitch.input, "live_123", "a stream key carries from one platform to another");
  assert.equal(twitch.name, "Twitch", "the name the last pick filled in follows the pick");
  const named = applyChoice({ ...twitch, name: "Main" }, by("Kick"), by("Twitch"));
  assert.equal(named.name, "Main", "a name the operator typed stays");
  const srt = applyChoice(named, by("Custom SRT"), by("Kick"));
  assert.equal(srt.protocol, "srt");
  assert.equal(srt.input, "");
});
