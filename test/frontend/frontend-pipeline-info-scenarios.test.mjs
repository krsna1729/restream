import assert from "node:assert/strict";

import { runDomScenarioMatrix } from "../support/helpers/ui-scenario-harness.mjs";

function makeOutput(overrides = {}) {
  return {
    id: "out-1",
    pipe: "pipe-1",
    name: "Primary Output",
    desiredState: "started",
    encoding: "source",
    url: "rtmp://example.com/live/secret",
    monitoringUrl: "https://example.com/monitor/out-1",
    status: "off",
    rawStatus: "off",
    phase: "idle",
    failurePhase: null,
    lastError: null,
    lastErrorAt: null,
    lastProgressAt: null,
    lastProgressAgeMs: null,
    retrying: false,
    retryAttempts: null,
    retryBackoffMs: null,
    nextRetryAt: null,
    retryRemainingMs: null,
    flapping: false,
    recentFailureCount: 0,
    time: null,
    job: null,
    totalSize: 0,
    bitrateKbps: null,
    ...overrides,
  };
}

function makePipeline(overrides = {}) {
  return {
    id: "pipe-1",
    name: "Pipeline 1",
    key: "stream-key",
    inputSource: null,
    fileIngest: null,
    ingestUrls: { rtmp: null, srt: null },
    input: {
      status: "off",
      time: null,
      probeReady: true,
      probeStatus: "ready",
      probePendingMs: null,
      video: null,
      videoTrackSelection: null,
      audio: null,
      audioTracks: [],
      bytesReceived: 0,
      bytesSent: 0,
      readers: 0,
      bitrateKbps: 0,
      publisher: null,
      unexpectedReadersCount: 0,
      lastSessionProtocol: null,
      lastDisconnectAt: null,
      lastDisconnectAgeMs: null,
      lastDisconnectReason: null,
      lastFailurePhase: null,
      recentDisconnectError: false,
      lastRemoteAddr: null,
      lastSessionBytesReceived: null,
    },
    outs: [makeOutput()],
    stats: {
      inputBitrateKbps: 0,
      outputBitrateKbps: 0,
      readerCount: 0,
      outputCount: 1,
      readerMismatch: false,
      unexpectedReadersCount: 0,
    },
    recording: { enabled: false, active: false },
    hlsPreview: {
      active: false,
      persistentConsumers: 0,
      lastAccessAgeMs: null,
      segments: 0,
      playlistBytes: 0,
    },
    ...overrides,
  };
}

function appendRoot(document, tagName, id) {
  const element = document.createElement(tagName);
  element.id = id;
  document.body.appendChild(element);
  return element;
}

function setupPipelineInfoDom(document) {
  appendRoot(document, "div", "pipe-info-col");
}

async function flushAsyncWork() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

runDomScenarioMatrix({
  suite: "pipeline info scenario matrix",
  setupDom({ document }) {
    setupPipelineInfoDom(document);
  },
  async loadModules({ loadCompiledFrontendModule }) {
    const pipelineView = await loadCompiledFrontendModule("features/pipeline-view/index.js");
    const pipelineDeps = await loadCompiledFrontendModule(
      "features/pipeline-dependencies.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");

    pipelineDeps.setPipelineViewDependencies({
      refreshDashboard: async () => {},
    });
    const headerModels = [];
    pipelineView.configurePipelineHeaderPresentation({
      onPresentation: (model) => {
        headerModels.push(model);
      },
    });
    const inputModels = [];
    pipelineView.configurePipelineInputStatusPresentation({
      onPresentation: (model) => {
        inputModels.push(model);
      },
    });

    return {
      headerModels,
      inputModels,
      renderPipelineInfoColumn: pipelineView.renderPipelineInfoColumn,
      state,
    };
  },
  scenarios: [
    {
      name: "probe pending live input shows a probing badge and keeps diagnostics available",
      async run({ headerModels, inputModels, renderPipelineInfoColumn, state }) {
        globalThis.fetch = undefined;
        state.pipelines = [
          makePipeline({
            input: {
              ...makePipeline().input,
              status: "on",
              time: 12_000,
              probeReady: false,
              probePendingMs: 2500,
            },
          }),
        ];

        renderPipelineInfoColumn("pipe-1");

        const header = headerModels.at(-1);
        const inputStatus = inputModels.at(-1);

        assert.equal(inputStatus.status.label, "Probing");
        assert.equal(inputStatus.previewEnabled, true);
        assert.equal(inputStatus.metricGroups.length, 2);
        assert.equal(header.canDiagnose, true);
      },
    },
    {
      name: "offline failure state surfaces last failure context and disables live-only actions",
      async run({ headerModels, inputModels, renderPipelineInfoColumn, state }) {
        globalThis.fetch = undefined;
        state.pipelines = [
          makePipeline({
            input: {
              ...makePipeline().input,
              status: "off",
              lastDisconnectAt: "2026-06-30T00:00:09Z",
              lastDisconnectAgeMs: 9000,
              lastDisconnectReason: "connection reset by peer",
              lastFailurePhase: "connect",
              lastSessionProtocol: "srt",
              recentDisconnectError: true,
            },
          }),
        ];

        renderPipelineInfoColumn("pipe-1");

        const header = headerModels.at(-1);
        const inputStatus = inputModels.at(-1);

        assert.equal(inputStatus.status.label, "Input offline");
        assert.equal(inputStatus.status.tone, "error");
        assert.match(inputStatus.status.detail, /connection reset by peer/);
        assert.equal(inputStatus.previewEnabled, false);
        assert.equal(inputStatus.metricGroups.length, 0);
        assert.equal(header.canDiagnose, false);
        assert.equal(header.recordingControl.disabled, true);
      },
    },
    {
      name: "file source analysis shows sparse GOP warnings without manual dashboard inspection",
      async run({ headerModels, inputModels, renderPipelineInfoColumn, state }) {
        globalThis.fetch = async (url) => {
          const href = String(url);
          if (href === "/api/v1/media") {
            return new Response(
              JSON.stringify({
                files: [
                  {
                    name: "session-recording.ts",
                    size: 4096,
                    modifiedAt: "2026-06-30T00:00:00Z",
                  },
                ],
              }),
              {
                status: 200,
                headers: { "content-type": "application/json" },
              },
            );
          }
          if (href === "/api/v1/media/session-recording.ts/analysis") {
            return new Response(
              JSON.stringify({
                videoCodec: "h264",
                fps: 29.97,
                durationSec: 62.4,
                keyframeCount: 10,
                averageKeyframeIntervalSec: 3,
                maxKeyframeIntervalSec: 6,
                sparseForLive: true,
                liveGopTargetSeconds: 2,
              }),
              {
                status: 200,
                headers: { "content-type": "application/json" },
              },
            );
          }
          throw new Error(`Unexpected fetch in test: ${href}`);
        };

        state.pipelines = [
          makePipeline({
            inputSource: "file:session-recording.ts",
            fileIngest: {
              configured: true,
              id: "ingest-1",
              filename: "session-recording.ts",
              running: false,
              loop: true,
              startTime: "00:00:05",
              liveOptimized: true,
              targetGopSeconds: 2,
            },
          }),
        ];

        renderPipelineInfoColumn("pipe-1");
        await flushAsyncWork();

        const header = headerModels.at(-1);
        const inputStatus = inputModels.at(-1);
        const details = new Map(
          inputStatus.fileSource.details.map((detail) => [
            detail.key,
            detail.value,
          ]),
        );

        assert.equal(header.fileIngestControl?.label, "Start File");
        assert.equal(inputStatus.liveSource, null);
        assert.equal(inputStatus.fileSource.filename, "session-recording.ts");
        assert.equal(details.get("container"), "MPEG-TS");
        assert.equal(details.get("loop"), "Enabled");
        assert.equal(details.get("start"), "00:00:05");
        assert.equal(details.get("optimization"), "Enabled (2s GOP)");
        assert.equal(details.get("gop"), "avg 3.0s | max 6.0s");
        assert.match(inputStatus.fileSource.warning, /Sparse source GOP detected/);
      },
    },
    {
      name: "recording lock keeps edit disabled while live-source ingest controls remain available",
      async run({ headerModels, inputModels, renderPipelineInfoColumn, state }) {
        globalThis.fetch = undefined;
        state.pipelines = [
          makePipeline({
            recording: { enabled: true, active: true },
            input: {
              ...makePipeline().input,
              status: "on",
            },
            ingestUrls: {
              rtmp: "rtmp://example.com/live/stream-key",
              srt: null,
            },
          }),
        ];

        renderPipelineInfoColumn("pipe-1");

        const header = headerModels.at(-1);
        const inputStatus = inputModels.at(-1);
        const rtmpProtocol = inputStatus.liveSource.protocols.find(
          ({ id }) => id === "rtmp",
        );

        assert.equal(header.canEdit, false);
        assert.match(header.editDisabledReason, /Stop recording before editing/);
        assert.equal(inputStatus.fileSource, null);
        assert.equal(inputStatus.liveSource.pipelineId, "pipe-1");
        assert.equal(inputStatus.liveSource.streamKeyLabel, "stream-key");
        assert.equal(rtmpProtocol.selected, true);
        assert.equal(rtmpProtocol.urlLabel, "rtmp://example.com/live/stream-key");
      },
    },
  ],
});
