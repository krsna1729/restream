import assert from "node:assert/strict";
import test from "node:test";

import * as channelsModel from "../../web/ts/control-room/channels-model.ts";

const { buildChannels, bulkTargets, channelState, platformOf, platformSummaries, runBulk } = channelsModel;

function output(id, overrides = {}) {
  return {
    id,
    pipelineId: "p1",
    name: id,
    protocol: "rtmp",
    shard: null,
    stage: null,
    desired: "running",
    actual: "sending",
    convergence: "converged",
    flow: "flowing",
    backpressure: null,
    retry: null,
    evidence: null,
    diagnosis: null,
    ...overrides,
  };
}

test("the platform comes from the URL's host; an unknown host is a custom destination", () => {
  assert.equal(platformOf("rtmp://a.rtmp.youtube.com/live2/key", "rtmp").id, "youtube");
  assert.equal(platformOf("rtmp://live.twitch.tv/app/key", "rtmp").id, "twitch");
  // Kick ingests on live-video.net too; its global endpoint is Kick, not Twitch.
  assert.equal(platformOf("rtmps://abc.global-contribute.live-video.net/app/key", "rtmp").id, "kick");
  assert.equal(platformOf("rtmps://live-api-s.facebook.com:443/rtmp/key", "rtmp").id, "facebook");
  assert.equal(platformOf("srt://ingest.cdn.example:9000", "srt").id, "custom-srt");
  assert.equal(platformOf("not a url", "hls").id, "custom-hls");
  assert.equal(platformOf(undefined, "moq").id, "moq");
});

test("channel state follows the operator's intent first, then the engine's convergence", () => {
  assert.equal(channelState(output("a")), "live");
  assert.equal(channelState(output("a", { desired: "stopped" })), "off");
  assert.equal(channelState(output("a", { convergence: "retrying" })), "retrying");
  assert.equal(channelState(output("a", { convergence: "retriesExhausted" })), "failed");
  assert.equal(channelState(output("a", { convergence: "waitingForIngest", flow: "idle" })), "waiting");
  assert.equal(
    channelState(output("a", { diagnosis: { layer: "destination", cause: { code: "noProgress" }, ruledOut: [] } })),
    "degraded",
  );
  assert.equal(channelState(output("a", { convergence: "starting", flow: "idle" })), "starting");
});

test("platform summaries and bulk targets", () => {
  const state = {
    epoch: "e",
    revision: 1,
    pipelines: [{ id: "p1", name: "Main", ingest: null, standbyInputs: 0, recording: false }],
    outputs: [
      output("yt1"),
      output("yt2", { convergence: "retrying" }),
      output("yt3", { desired: "stopped" }),
      output("tw1", { convergence: "retriesExhausted" }),
    ],
    inputs: [],
    graphs: [],
  };
  const urls = new Map([
    ["yt1", "rtmp://a.rtmp.youtube.com/live2/1"],
    ["yt2", "rtmp://a.rtmp.youtube.com/live2/2"],
    ["yt3", "rtmp://a.rtmp.youtube.com/live2/3"],
    ["tw1", "rtmp://live.twitch.tv/app/1"],
  ]);
  const channels = buildChannels(state, urls);
  assert.equal(channels[0].pipelineName, "Main");
  assert.deepEqual(
    platformSummaries(channels).map((summary) => [summary.platform.id, summary.total, summary.live, summary.troubled]),
    [
      ["youtube", 3, 1, 1],
      ["twitch", 1, 0, 1],
    ],
  );
  const ids = (action) => bulkTargets(channels, action).map((channel) => channel.output.id);
  assert.deepEqual(ids("start"), ["yt3"]);
  assert.deepEqual(ids("stop"), ["yt1", "yt2", "tw1"]);
  assert.deepEqual(ids("retry"), ["yt2", "tw1"]);
});

test("a bulk run keeps its concurrency bound and counts failures without stopping", async () => {
  let inFlight = 0;
  let peak = 0;
  const progress = [];
  const result = await runBulk(
    [1, 2, 3, 4, 5, 6, 7],
    async (item) => {
      inFlight += 1;
      peak = Math.max(peak, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 5));
      inFlight -= 1;
      if (item === 3) throw new Error("refused");
      return item !== 5;
    },
    3,
    (done, failed) => progress.push([done, failed]),
  );
  assert.deepEqual(result, { done: 7, failed: 2 });
  assert.equal(peak, 3);
  assert.deepEqual(progress.at(-1), [7, 2]);
  assert.deepEqual(await runBulk([], async () => true, 4, () => {}), { done: 0, failed: 0 });
});

test("a bulk action names what it touches: top platforms and pipelines", () => {
  const { impactWords } = channelsModel;
  const make = (id, platformId, label, pipelineId) => ({ output: { id, pipelineId }, platform: { id: platformId, label } });
  const channels = [
    make("a", "yt", "YouTube", "p1"),
    make("b", "yt", "YouTube", "p2"),
    make("c", "tw", "Twitch", "p1"),
    make("d", "fb", "Facebook", "p3"),
    make("e", "kk", "Kick", "p3"),
  ];
  assert.equal(impactWords(channels), "YouTube 2 · Facebook 1 · Kick 1 · 1 more platform, across 3 pipelines");
  assert.equal(impactWords(channels.slice(0, 1)), "YouTube 1, across 1 pipeline");
});
