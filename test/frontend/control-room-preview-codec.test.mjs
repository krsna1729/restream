import assert from "node:assert/strict";
import { test } from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { hlsDecodesHevc, isHevc, moqUndecodableReason, programPreviewChoice } =
  await loadCompiledFrontendModule("control-room/preview-codec.js");

const URLS = { source: "/hls/p.hevc/master.m3u8", h264: "/hls/p/master.m3u8" };
const mse = (supported) => ({
  isTypeSupported: (type) => supported && type.includes("hvc1"),
});

test("an H.265 source plays as sent where the browser decodes H.265, with the H.264 copy behind it", () => {
  assert.deepEqual(programPreviewChoice(URLS, "hevc", true), {
    url: URLS.source,
    fallbackUrl: URLS.h264,
  });
  assert.deepEqual(programPreviewChoice(URLS, "h265", false), {
    url: URLS.h264,
    fallbackUrl: null,
  });
  // H.264 sources and unknown codecs always take the plain preview.
  assert.deepEqual(programPreviewChoice(URLS, "h264", true), {
    url: URLS.h264,
    fallbackUrl: null,
  });
  assert.deepEqual(programPreviewChoice(URLS, null, true), {
    url: URLS.h264,
    fallbackUrl: null,
  });
  assert.ok(isHevc("HEVC") && isHevc("h.265") && !isHevc("h264"));
});

test("H.265 support is read from Media Source Extensions, Managed MSE or native playback", () => {
  assert.equal(hlsDecodesHevc({ MediaSource: mse(true) }), true);
  assert.equal(hlsDecodesHevc({ MediaSource: mse(false) }), false);
  assert.equal(hlsDecodesHevc({ ManagedMediaSource: mse(true) }), true);
  const video = (answer) => ({
    createElement: () => ({ canPlayType: () => answer }),
  });
  assert.equal(
    hlsDecodesHevc({ MediaSource: mse(false), document: video("maybe") }),
    true,
  );
  assert.equal(hlsDecodesHevc({ document: video("") }), false);
  assert.equal(
    hlsDecodesHevc({
      MediaSource: {
        isTypeSupported: () => {
          throw new Error("x");
        },
      },
    }),
    false,
  );
  assert.equal(hlsDecodesHevc({}), false);
});

test("MoQ refuses an H.265 source at once where WebCodecs cannot decode it", async () => {
  const decoder = (supported) => ({
    VideoDecoder: { isConfigSupported: async () => ({ supported }) },
  });
  assert.equal(
    await moqUndecodableReason("h264", decoder(false)),
    null,
    "only H.265 is checked",
  );
  assert.equal(await moqUndecodableReason("hevc", decoder(true)), null);
  assert.match(
    await moqUndecodableReason("hevc", decoder(false)),
    /cannot decode H\.265.*use HLS/,
  );
  assert.match(await moqUndecodableReason("hevc", {}), /cannot decode H\.265/);
});
