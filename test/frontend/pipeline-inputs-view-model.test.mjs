import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

function pipelineInput(overrides = {}) {
  return {
    id: "input-a",
    pipelineId: "pipeline-a",
    label: "Encoder A",
    streamKey: "sk_test",
    role: "primary",
    enabled: true,
    selected: true,
    ingestUrls: { rtmp: null, srt: null },
    previewUrl: "/hls/inputs/input-a/master.m3u8",
    runtime: {
      connected: false,
      forwardingState: null,
      protocol: null,
      uptimeSeconds: null,
      bytesReceived: 0,
      remoteAddr: null,
      video: null,
      audio: null,
      quality: null,
    },
    ...overrides,
  };
}

test("pipeline input status distinguishes forwarding, warm standby, and offline", async () => {
  const { pipelineInputStatusLabel } = await loadCompiledFrontendModule(
    "features/pipeline-inputs-view-model.js",
  );

  assert.equal(
    pipelineInputStatusLabel(
      pipelineInput({
        runtime: {
          ...pipelineInput().runtime,
          connected: true,
          forwardingState: "active",
        },
      }),
    ),
    "Forwarding",
  );
  assert.equal(
    pipelineInputStatusLabel(
      pipelineInput({
        selected: false,
        role: "backup",
        runtime: {
          ...pipelineInput().runtime,
          connected: true,
          forwardingState: "standby",
        },
      }),
    ),
    "Connected standby",
  );
  assert.equal(
    pipelineInputStatusLabel(
      pipelineInput({ selected: false, role: "backup" }),
    ),
    "Offline",
  );
});

test("pipeline input subtitle exposes selection, protocol, and received bytes", async () => {
  const { pipelineInputSubtitle } = await loadCompiledFrontendModule(
    "features/pipeline-inputs-view-model.js",
  );
  const input = pipelineInput({
    selected: false,
    role: "backup",
    runtime: {
      ...pipelineInput().runtime,
      connected: true,
      forwardingState: "standby",
      protocol: "srt",
      bytesReceived: 1_572_864,
    },
  });

  assert.equal(
    pipelineInputSubtitle(input),
    "Standby · SRT · 1.5 MB received",
  );
});
