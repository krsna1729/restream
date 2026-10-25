import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { encoderIdentity, declaredWords, declaredVerdicts } = await loadCompiledFrontendModule("control-room/encoder-model.js");

test("encoders read as products people know, with their version", () => {
  const cases = [
    ["obs-output module (libobs version 30.2.3)", "obs", "OBS Studio", "30.2.3"],
    ["Streamlabs Desktop (libobs version 30.1.2)", "streamlabs", "Streamlabs", "30.1.2"],
    ["Lavf60.16.100", "ffmpeg", "FFmpeg", "6.x"],
    ["Lavf62.3.100", "ffmpeg", "FFmpeg", "8.x"],
    ["vMix/27.0.0.49", "vmix", "vMix", "27.0.0.49"],
    ["Wirecast/15.3.1", "wirecast", "Wirecast", "15.3.1"],
    ["Larix Broadcaster 1.3.12", "larix", "Larix Broadcaster", "1.3.12"],
    ["GStreamer FLV muxer", "gstreamer", "GStreamer", null],
    ["Blackmagic ATEM Mini Pro", "blackmagic", "Blackmagic", null],
  ];
  for (const [raw, id, name, version] of cases) {
    const identity = encoderIdentity(raw);
    assert.deepEqual([identity.id, identity.name, identity.version], [id, name, version], raw);
    assert.equal(identity.raw, raw);
  }
  const unknown = encoderIdentity("  SomeBox Encoder 2000 with a rather long product name  ");
  assert.equal(unknown.id, "other");
  assert.equal(unknown.name, "SomeBox Encoder 2000 with a rather long…");
  assert.equal(unknown.version, null, "an unknown name keeps its text, no guessed version");
});

test("the declared settings in words, from whatever was declared", () => {
  assert.equal(
    declaredWords({ width: 1920, height: 1080, frameRate: 59.94, videoKbps: 6000, audioKbps: 160 }),
    "1920×1080 · 59.94 fps · 6.0 Mb/s video + 160 kb/s audio",
  );
  assert.equal(declaredWords({ videoKbps: 800 }), "800 kb/s video");
  assert.equal(declaredWords({ encoder: "x" }), "");
});

function sample(atMs, { bitrateBps = 6_200_000, fps = 60, width = 1920, height = 1080 } = {}) {
  return {
    atMs,
    ring: null,
    outputs: {},
    ingest: {
      inputId: "in",
      protocol: "rtmp",
      bitrateBps,
      packetsPerSec: 100,
      width,
      height,
      timeline: { windowSec: 60, unread: 0, tracks: [{ media: "video", track: 0, packetsPerSec: fps, backwards: 0, jumps: 0, ptsBeforeDts: 0 }] },
    },
  };
}
const OBS = { encoder: "obs-output module (libobs version 30.2.3)", width: 1920, height: 1080, frameRate: 60, videoKbps: 6000, audioKbps: 160 };

test("what arrives as declared says nothing", () => {
  assert.deepEqual(declaredVerdicts(OBS, Array.from({ length: 10 }, (_, i) => sample(i))), []);
  assert.deepEqual(declaredVerdicts(null, [sample(0)]), [], "no declaration, no comparison");
  assert.deepEqual(declaredVerdicts(OBS, [sample(0, { bitrateBps: 100 })]), [], "too few readings to judge");
});

test("a bitrate or frame rate short of the declaration is named, worse is bad", () => {
  const short = declaredVerdicts(OBS, Array.from({ length: 10 }, (_, i) => sample(i, { bitrateBps: 4_000_000, fps: 50 })));
  const byKey = Object.fromEntries(short.map((verdict) => [verdict.key, verdict]));
  assert.equal(byKey["declared-bitrate"].severity, "warn");
  assert.equal(byKey["declared-bitrate"].title, "4.0 Mb/s arriving of the 6.2 Mb/s OBS Studio is set to send");
  assert.equal(byKey["declared-fps"].severity, "warn");
  assert.equal(byKey["declared-fps"].title, "50 fps arriving of the 60 fps OBS Studio is set to");
  const bad = declaredVerdicts(OBS, Array.from({ length: 10 }, (_, i) => sample(i, { bitrateBps: 2_000_000, fps: 30 })));
  assert.deepEqual(bad.map((verdict) => verdict.severity), ["bad", "bad"]);
});

test("a picture size other than declared is named", () => {
  const verdicts = declaredVerdicts(OBS, Array.from({ length: 10 }, (_, i) => sample(i, { width: 1280, height: 720 })));
  assert.deepEqual(verdicts.map((verdict) => verdict.key), ["declared-size"]);
  assert.equal(verdicts[0].title, "OBS Studio declares 1920×1080 but sends 1280×720");
});
