import assert from "node:assert/strict";
import test from "node:test";

import { installFakeDom } from "../support/helpers/fake-dom.mjs";

function makeResponse(payload, status = 200) {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json" },
  });
}

test("audio caps load and detection logic normalizes payloads and URL inference", async () => {
  installFakeDom();
  globalThis.fetch = async (url) => {
    assert.equal(String(url), "/api/v1/audio-caps");
    return makeResponse({
      caps: {
        "youtube:rtmp": { maxTracks: 2, maxChannels: null, codecs: ["aac"] },
        "generic:hls": { maxTracks: null, maxChannels: 6, codecs: null },
      },
      platformLabels: {
        youtube: "YouTube",
        facebook: "Facebook Live",
        vdocipher: "VdoCipher",
        generic: "Everywhere",
      },
    });
  };

  const audioCaps = await import("../../web/ts/core/audio-caps.ts");

  assert.equal(audioCaps.isAudioCapsLoaded(), false);
  await audioCaps.loadAudioCaps();

  assert.equal(audioCaps.isAudioCapsLoaded(), true);
  assert.deepEqual(audioCaps.getAudioCaps("youtube", "rtmp"), {
    maxTracks: 2,
    maxChannels: Infinity,
    codecs: ["aac"],
  });
  assert.deepEqual(audioCaps.getAudioCaps("generic", "hls"), {
    maxTracks: Infinity,
    maxChannels: 6,
    codecs: "any",
  });
  assert.equal(audioCaps.getAudioPlatformLabel("generic"), "Everywhere");
  assert.equal(
    audioCaps.detectAudioPlatform("https://live.vd0.co/channel/test"),
    "vdocipher",
  );
  assert.equal(
    audioCaps.detectAudioProtocol("https://example.com/live/out.m3u8"),
    "hls",
  );
  assert.equal(audioCaps.detectAudioProtocol("bad-url", "srt"), "srt");
});

test("editor output-url helpers resolve presets, protocols, and SRT fields", async () => {
  const outputUrl = await import("../../web/ts/core/output-url.ts");

  assert.equal(outputUrl.protocolUsesOutputServerPresets("hls"), true);
  assert.equal(
    outputUrl.resolvePresetOutputUrl(
      "https://a.upload.youtube.com/http_upload_hls?cid=${stream_key}",
      "stream key",
    ),
    "https://a.upload.youtube.com/http_upload_hls?cid=stream%20key",
  );
  assert.deepEqual(
    outputUrl.matchOutputServerPreset(
      "rtmp",
      "rtmp://a.rtmp.youtube.com/live2/abc123",
    ),
    {
      value: "rtmp://a.rtmp.youtube.com/live2/",
      inputValue: "abc123",
      rtmpMode: "enhanced",
    },
  );
  assert.deepEqual(
    outputUrl.matchOutputServerPreset(
      "rtmp",
      "rtmp://b.rtmp.youtube.com/live2?backup=1/backup-key",
    ),
    {
      value: "rtmp://b.rtmp.youtube.com/live2?backup=1/",
      inputValue: "backup-key",
      rtmpMode: "enhanced",
    },
  );
  assert.deepEqual(
    outputUrl.matchOutputServerPreset(
      "rtmp",
      "rtmps://live-api-s.facebook.com:443/rtmp/abc123",
    ),
    {
      value: "rtmps://live-api-s.facebook.com:443/rtmp/",
      inputValue: "abc123",
    },
  );
  assert.equal(
    outputUrl.detectOutputProtocol("https://example.com/live/out.m3u8"),
    "hls",
  );
  assert.equal(
    outputUrl.extractCandidateStreamToken(
      "srt://example.com:9000?streamid=publish:main-feed",
    ),
    "main-feed",
  );
  assert.equal(
    outputUrl.getDefaultOutputToken("https://example.com/hls/show/out.m3u8"),
    "show",
  );
  assert.deepEqual(
    outputUrl.parseSrtFields(
      "srt://example.com:10080?streamid=publish:feed&passphrase=supersecret1&pbkeylen=24&latency=200",
    ),
    {
      host: "example.com",
      port: "10080",
      streamId: "publish:feed",
      passphrase: "supersecret1",
      pbkeylen: "24",
      extraQuery: "latency=200",
    },
  );
  assert.equal(
    outputUrl.buildDefaultCustomOutputUrl(
      "rtmp",
      "rtmp://seed/live/key",
      "demo",
    ),
    "rtmp://demo:1935/live/key",
  );
});
