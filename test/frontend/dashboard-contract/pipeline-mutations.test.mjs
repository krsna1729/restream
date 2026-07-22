import assert from "node:assert/strict";
import test from "node:test";

import {
  appendRoot,
  flushAsyncWork,
  installFakeDom,
  loadCompiledFrontendModule,
  waitForCondition,
} from "./helpers.mjs";

test("pipeline edits reuse returned pipeline payloads instead of refetching dashboard settings", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const streamKeysUrl = "/api/v1/stream-keys";
  const updatePipelineUrl = "/api/v1/pipelines/pipe-1";
  const mediaUrl = "/api/v1/media";
  const mediaAnalysisUrl = "/api/v1/media/recording-1.ts/analysis";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-v2-operate-panel");

  const appendField = (tagName, id, value = "") => {
    const element = document.createElement(tagName);
    element.id = id;
    element.value = value;
    document.body.appendChild(element);
    return element;
  };

  const modal = appendField("dialog", "edit-pipe-modal");
  modal.close = () => {};
  modal.showModal = () => {};
  appendField("input", "pipe-mode-input", "edit");
  appendField("input", "pipe-id-input", "pipe-1");
  appendField("input", "pipe-name-input", "Edited Pipeline");
  appendField("select", "pipe-source-type-input", "file");
  appendField("select", "pipe-file-input", "recording-1.ts");
  appendField("select", "pipe-srt-ingest-mode-input", "inherit");
  appendField("input", "pipe-srt-ingest-passphrase-input", "");
  appendField("select", "pipe-srt-ingest-pbkeylen-input", "16");
  appendField("select", "pipe-stream-key-input", "stream-key");
  appendField("input", "pipe-file-start-time-input", "00:00:05");
  appendField("input", "pipe-file-gop-seconds-input", "3");
  appendField("input", "pipe-file-loop-input").checked = true;
  appendField("input", "pipe-file-live-optimized-input").checked = true;
  appendRoot(document, "div", "pipe-file-fields");
  appendRoot(document, "details", "pipe-srt-ingest-fields");
  appendRoot(document, "div", "pipe-file-analysis-summary");
  appendRoot(document, "div", "pipe-file-warning").classList.add("hidden");
  appendRoot(document, "div", "pipe-stream-key-locked-hint").classList.add(
    "hidden",
  );
  appendRoot(document, "div", "pipe-modal-title");
  appendField("button", "pipe-submit-btn");

  const requests = [];
  globalThis.fetch = async (url, options = {}) => {
    const href = String(url);
    const method = String(options.method || "GET").toUpperCase();
    requests.push([method, href]);

    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          ingestHost: "stream.example.com",
          pipelines: [
            {
              id: "pipe-1",
              name: "Pipeline 1",
              streamKey: "stream-key",
              inputSource: null,
              fileIngest: {
                configured: false,
                running: false,
              },
            },
          ],
          outputs: [],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === streamKeysUrl) {
      return new Response(
        JSON.stringify([
          {
            key: "stream-key",
            label: "Stream 1",
            ingestUrls: {
              rtmp: "rtmp://stream.example.com:1935/live/stream-key",
              srt: "srt://stream.example.com:10080?streamid=publish:stream-key",
            },
          },
        ]),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === fullRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "off",
                  probeReady: false,
                  probeStatus: "off",
                  bytesReceived: 0,
                  bytesSent: 0,
                  readers: 0,
                  bitrateKbps: null,
                },
                outputs: {},
              },
            },
          },
          metrics: {
            generatedAt: "2026-06-30T00:00:00Z",
            cpu: { usagePercent: 12 },
            memory: { usedPercent: 20 },
            disk: { usedPercent: 40 },
            network: { downloadKbps: 1, uploadKbps: 2 },
            engine: {
              cpuPercent: 3,
              totalMemoryBytes: 1234,
              cpuSampleReady: true,
            },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === mediaUrl) {
      return new Response(
        JSON.stringify({
          files: [{ name: "recording-1.ts", kind: "recording" }],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === mediaAnalysisUrl) {
      return new Response(
        JSON.stringify({
          videoCodec: "h264",
          fps: 30,
          durationSec: 60,
          averageKeyframeIntervalSec: 2,
          maxKeyframeIntervalSec: 2,
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === updatePipelineUrl && method === "PATCH") {
      return new Response(
        JSON.stringify({
          message: "Pipeline updated",
          pipeline: {
            id: "pipe-1",
            name: "Edited Pipeline",
            streamKey: "stream-key",
            inputSource: "file:recording-1.ts",
            srtIngestPolicy: { mode: "inherit", passphrase: null, pbkeylen: null },
            ingestUrls: {
              rtmp: "rtmp://stream.example.com:1935/live/stream-key",
              srt: "srt://stream.example.com:10080?streamid=publish:stream-key",
            },
            fileIngest: {
              configured: true,
              id: "ingest-1",
              filename: "recording-1.ts",
              streamKey: "stream-key",
              loop: true,
              startTime: "00:00:05",
              liveOptimized: true,
              targetGopSeconds: 3,
              running: false,
            },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${method} ${href}`);
  };

  const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
  const editor = await loadCompiledFrontendModule("features/editor/index.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");

  await dashboard.refreshDashboard();
  await editor.editPipeBtn();
  await flushAsyncWork();
  document.getElementById("pipe-source-type-input").value = "file";
  document.getElementById("pipe-file-input").value = "recording-1.ts";
  document.getElementById("pipe-file-start-time-input").value = "00:00:05";
  document.getElementById("pipe-file-gop-seconds-input").value = "3";
  document.getElementById("pipe-file-loop-input").checked = true;
  document.getElementById("pipe-file-live-optimized-input").checked = true;
  requests.length = 0;

  await editor.pipeFormBtn({ preventDefault() {} });
  await flushAsyncWork();

  const mutationRequests = requests.filter(([method]) => method === "PATCH");
  assert.deepEqual(mutationRequests, [["PATCH", updatePipelineUrl]]);
  assert.equal(
    requests.some(([, href]) => href === settingsUrl),
    false,
    "editing a pipeline should not refetch dashboard settings when the API returned the updated pipeline",
  );
  assert.equal(
    requests.some(([, href]) => href.endsWith("/file-ingest")),
    false,
    "editing a pipeline should reuse inline file-ingest state instead of issuing a second file-ingest mutation",
  );
  assert.equal(state.config.pipelines[0].name, "Edited Pipeline");
  assert.equal(state.config.pipelines[0].inputSource, "file:recording-1.ts");
  assert.equal(state.config.pipelines[0].fileIngest?.configured, true);
  assert.equal(state.config.pipelines[0].fileIngest?.filename, "recording-1.ts");
});

test("pipeline edit modal defers media file lookups until file mode is selected", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const streamKeysUrl = "/api/v1/stream-keys";
  const mediaUrl = "/api/v1/media";
  const mediaAnalysisUrl = "/api/v1/media/recording-1.ts/analysis";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-v2-operate-panel");

  const appendField = (tagName, id, value = "") => {
    const element = document.createElement(tagName);
    element.id = id;
    element.value = value;
    document.body.appendChild(element);
    return element;
  };

  const modal = appendField("dialog", "edit-pipe-modal");
  modal.close = () => {};
  modal.showModal = () => {};
  appendField("input", "pipe-mode-input", "edit");
  appendField("input", "pipe-id-input", "pipe-1");
  appendField("input", "pipe-name-input", "Pipeline 1");
  appendField("select", "pipe-source-type-input", "publisher");
  appendField("select", "pipe-file-input", "");
  appendField("select", "pipe-srt-ingest-mode-input", "inherit");
  appendField("input", "pipe-srt-ingest-passphrase-input", "");
  appendField("select", "pipe-srt-ingest-pbkeylen-input", "16");
  appendField("select", "pipe-stream-key-input", "stream-key");
  appendField("input", "pipe-file-start-time-input", "00:00:00");
  appendField("input", "pipe-file-gop-seconds-input", "2");
  appendField("input", "pipe-file-loop-input").checked = false;
  appendField("input", "pipe-file-live-optimized-input").checked = false;
  appendRoot(document, "div", "pipe-file-fields");
  appendRoot(document, "details", "pipe-srt-ingest-fields");
  appendRoot(document, "div", "pipe-file-analysis-summary");
  appendRoot(document, "div", "pipe-file-warning").classList.add("hidden");
  appendRoot(document, "div", "pipe-stream-key-locked-hint").classList.add(
    "hidden",
  );
  appendRoot(document, "div", "pipe-modal-title");
  appendField("button", "pipe-submit-btn");

  const requests = [];
  globalThis.fetch = async (url) => {
    const href = String(url);
    requests.push(href);

    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          ingestHost: "stream.example.com",
          pipelines: [
            {
              id: "pipe-1",
              name: "Pipeline 1",
              streamKey: "stream-key",
              inputSource: null,
              fileIngest: {
                configured: false,
                running: false,
              },
            },
          ],
          outputs: [],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === streamKeysUrl) {
      return new Response(
        JSON.stringify([
          {
            key: "stream-key",
            label: "Stream 1",
            ingestUrls: {
              rtmp: "rtmp://stream.example.com:1935/live/stream-key",
              srt: "srt://stream.example.com:10080?streamid=publish:stream-key",
            },
          },
        ]),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === fullRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "off",
                  probeReady: false,
                  probeStatus: "off",
                  bytesReceived: 0,
                  bytesSent: 0,
                  readers: 0,
                  bitrateKbps: null,
                },
                outputs: {},
              },
            },
          },
          metrics: {
            generatedAt: "2026-06-30T00:00:00Z",
            cpu: { usagePercent: 12 },
            memory: { usedPercent: 20 },
            disk: { usedPercent: 40 },
            network: { downloadKbps: 1, uploadKbps: 2 },
            engine: {
              cpuPercent: 3,
              totalMemoryBytes: 1234,
              cpuSampleReady: true,
            },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === mediaUrl) {
      return new Response(
        JSON.stringify({
          files: [{ name: "recording-1.ts", kind: "recording" }],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === mediaAnalysisUrl) {
      return new Response(
        JSON.stringify({
          videoCodec: "h264",
          fps: 30,
          durationSec: 60,
          averageKeyframeIntervalSec: 2,
          maxKeyframeIntervalSec: 2,
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${href}`);
  };

  const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
  const editor = await loadCompiledFrontendModule("features/editor/index.js");

  await dashboard.refreshDashboard();
  await editor.editPipeBtn();
  await flushAsyncWork();

  assert.equal(
    requests.includes(mediaUrl),
    false,
    "publisher pipeline edits should not preload media file lists",
  );
  assert.equal(
    requests.includes(mediaAnalysisUrl),
    false,
    "publisher pipeline edits should not analyze media files until file mode is used",
  );

  const sourceTypeInput = document.getElementById("pipe-source-type-input");
  sourceTypeInput.value = "file";
  sourceTypeInput.onchange();
  await flushAsyncWork();
  await flushAsyncWork();

  assert.equal(
    requests.includes(mediaUrl),
    true,
    "switching into file mode should lazily load the file list",
  );
  assert.equal(
    requests.includes(mediaAnalysisUrl),
    false,
    "analysis should still wait until a file is actually selected",
  );

  const fileInput = document.getElementById("pipe-file-input");
  fileInput.value = "recording-1.ts";
  fileInput.onchange();
  await flushAsyncWork();
  await flushAsyncWork();

  assert.equal(
    requests.includes(mediaAnalysisUrl),
    true,
    "selecting a file should fetch analysis on demand",
  );
});

test("recording patches local state immediately, while file-ingest falls back to runtime refresh when no lifecycle stream is open", async () => {
  const settingsUrl = "/api/v1/settings?view=dashboard";
  const fullRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=full&pipeline_id=pipe-1";
  const steadyRuntimeUrl =
    "/api/v1/dashboard/runtime?health_view=summary&metrics_view=summary&pipeline_id=pipe-1";
  const startRecordingUrl = "/api/v1/pipelines/pipe-1/recording/start";
  const startIngestUrl = "/api/v1/ingests/ingest-1/start";
  const { document, window } = installFakeDom();
  window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
  appendRoot(document, "div", "dashboard-v2-operate-panel");
  appendRoot(document, "div", "pipe-info-col");

  const requests = [];
  let resolveStartRecordingRequest;
  const startRecordingResponseReady = new Promise((resolve) => {
    resolveStartRecordingRequest = resolve;
  });
  let resolveStartIngestRequest;
  const startIngestResponseReady = new Promise((resolve) => {
    resolveStartIngestRequest = resolve;
  });
  globalThis.fetch = async (url, options = {}) => {
    const href = String(url);
    const method = String(options.method || "GET").toUpperCase();
    requests.push([method, href]);

    if (href === settingsUrl) {
      return new Response(
        JSON.stringify({
          serverName: "Restream",
          ingestHost: "stream.example.com",
          pipelines: [
            {
              id: "pipe-1",
              name: "Pipeline 1",
              streamKey: "stream-key",
              inputSource: "file:session-recording.ts",
              fileIngest: {
                configured: true,
                id: "ingest-1",
                filename: "session-recording.ts",
                running: false,
              },
              ingestUrls: {
                rtmp: "rtmp://example.com/live/stream-key",
                srt: "srt://example.com:10080?streamid=publish:stream-key",
              },
            },
          ],
          outputs: [],
          jobs: [],
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === fullRuntimeUrl || href === steadyRuntimeUrl) {
      return new Response(
        JSON.stringify({
          health: {
            status: "ready",
            pipelines: {
              "pipe-1": {
                input: {
                  status: "on",
                  probeReady: true,
                  probeStatus: "ready",
                  bytesReceived: 0,
                  bytesSent: 0,
                  readers: 0,
                  bitrateKbps: 3200,
                },
                recording: { enabled: false, active: false },
                outputs: {},
              },
            },
          },
          metrics: {
            generatedAt:
              href === fullRuntimeUrl
                ? "2026-06-30T00:00:00Z"
                : "2026-06-30T00:00:05Z",
            cpu: { usagePercent: 12 },
            memory: { usedPercent: 20 },
            disk: { usedPercent: 40 },
            network: { downloadKbps: 1, uploadKbps: 2 },
            engine: {
              cpuPercent: 3,
              totalMemoryBytes: 1234,
              cpuSampleReady: true,
            },
          },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === startRecordingUrl) {
      await startRecordingResponseReady;
      return new Response(
        JSON.stringify({ enabled: true, active: true }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    if (href === startIngestUrl) {
      await startIngestResponseReady;
      return new Response(
        JSON.stringify({
          id: "ingest-1",
          filename: "session-recording.ts",
          streamKey: "stream-key",
          loop: false,
          startTime: "00:00:00",
          liveOptimized: false,
          targetGopSeconds: 2,
          running: true,
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    }

    throw new Error(`Unexpected fetch: ${method} ${href}`);
  };

  const dashboard = await loadCompiledFrontendModule("features/dashboard.js");
  const pipelineView = await loadCompiledFrontendModule("features/pipeline-view/index.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");
  const headerModels = [];

  pipelineView.configurePipelineHeaderPresentation({
    onPresentation: (model) => {
      headerModels.push(model);
    },
  });
  pipelineView.setPipelineViewDependencies({
    refreshDashboardRuntime: dashboard.refreshDashboardRuntime,
    awaitDashboardRuntimeMutationConvergence:
      dashboard.awaitDashboardRuntimeMutationConvergence,
    updateDashboardPipelineFileIngestState:
      dashboard.updateDashboardPipelineFileIngestState,
    updateDashboardPipelineRecordingState:
      dashboard.updateDashboardPipelineRecordingState,
  });
  await dashboard.refreshDashboard();
  pipelineView.renderPipelineInfoColumn("pipe-1");
  requests.length = 0;

  const startRecordingPromise = pipelineView.togglePipelineRecording("pipe-1");
  await flushAsyncWork();
  const startingRecordingHeader = headerModels.at(-1);

  assert.equal(startingRecordingHeader.recordingControl.label, "Starting...");
  assert.equal(startingRecordingHeader.recordingControl.disabled, true);
  assert.deepEqual(requests, [["POST", startRecordingUrl]]);

  resolveStartRecordingRequest();
  await startRecordingPromise;
  await flushAsyncWork();

  assert.deepEqual(requests, [["POST", startRecordingUrl]]);
  assert.equal(state.pipelines[0].recording.enabled, true);
  assert.equal(state.pipelines[0].recording.active, true);
  const recordingHeader = headerModels.at(-1);
  assert.equal(
    recordingHeader.canEdit,
    false,
    "active recording should lock pipeline editing immediately from the mutation response",
  );

  document.hidden = true;
  dashboard.syncDashboardRuntimeStream();
  pipelineView.renderPipelineInfoColumn("pipe-1");
  requests.length = 0;

  const startIngestPromise = pipelineView.togglePipelineFileIngest("pipe-1");
  await flushAsyncWork();
  const startingIngestHeader = headerModels.at(-1);

  assert.equal(
    startingIngestHeader.fileIngestControl?.label,
    "Starting File...",
  );
  assert.equal(startingIngestHeader.fileIngestControl?.disabled, true);
  assert.deepEqual(requests, [["POST", startIngestUrl]]);

  resolveStartIngestRequest();
  await startIngestPromise;
  await flushAsyncWork();

  const runningIngestHeader = headerModels.at(-1);
  assert.equal(runningIngestHeader.fileIngestControl?.label, "Stop File");
  assert.equal(runningIngestHeader.fileIngestControl?.disabled, false);
  assert.equal(state.config.pipelines[0].fileIngest?.running, true);
  assert.deepEqual(requests, [
    ["POST", startIngestUrl],
    ["GET", steadyRuntimeUrl],
  ]);
  assert.equal(
    requests.some(([, href]) => href === settingsUrl),
    false,
    "pipeline runtime controls should not refetch dashboard settings after steady-state boot",
  );
});
