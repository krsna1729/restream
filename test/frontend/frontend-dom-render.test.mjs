import assert from "node:assert/strict";
import test from "node:test";

import {
  FakeElement,
  installFakeDom,
  loadCompiledFrontendModule,
} from "../support/helpers/fake-dom.mjs";

function makeOutput(overrides = {}) {
  return {
    id: "out-1",
    pipe: "pipe-1",
    name: "Primary Output",
    desiredState: "started",
    encoding: "source",
    url: "rtmp://example.com/live/secret",
    monitoringUrl: "https://example.com/monitor/out-1",
    status: "running",
    rawStatus: "running",
    phase: "sending",
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
    time: 15_000,
    job: null,
    totalSize: 2 * 1024 * 1024,
    bitrateKbps: 1500,
    ...overrides,
  };
}

function makePipeline(overrides = {}) {
  return {
    id: "pipe-1",
    name: "Pipeline 1",
    key: "stream-key",
    inputSource: null,
    ingestUrls: { rtmp: null, srt: null },
    input: {
      status: "on",
      time: 30_000,
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
      bitrateKbps: 3200,
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
      inputBitrateKbps: 3200,
      outputBitrateKbps: 1500,
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

function appendDashboardV2Roots(document) {
  for (const id of [
    "dashboard-v2-root",
    "dashboard-v2-pipeline-selector-root",
    "dashboard-v2-pipeline-header-root",
    "dashboard-v2-pipeline-input-status-root",
    "dashboard-v2-pipeline-output-overview-root",
    "dashboard-v2-pipeline-inspect-root",
    "dashboard-v2-pipeline-inspect-content",
    "dashboard-v2-control-room-root",
    "dashboard-v2-control-room-content",
    "dashboard-v2-media-root",
    "dashboard-v2-media-content",
    "dashboard-v2-settings-root",
    "dashboard-v2-settings-content",
    "dashboard-v2-status-root",
    "dashboard-v2-status-content",
    "dashboard-v2-incidents-root",
    "dashboard-v2-incidents-content",
    "dashboard-v2-telemetry-root",
    "dashboard-v2-telemetry-content",
  ]) {
    appendRoot(document, "div", id);
  }
}

function runCheck(name, fn) {
  test(name, { concurrency: false }, fn);
}

function appendInspectDom(document) {
  appendRoot(document, "section", "overview-mode-panel");
  appendRoot(document, "div", "overview-mode-content");
  appendRoot(document, "section", "control-mode-panel");
  appendRoot(document, "div", "control-mode-content");
  appendRoot(document, "section", "inspect-mode-panel");
  appendRoot(document, "section", "media-mode-panel");
  appendRoot(document, "div", "media-mode-content");
  appendRoot(document, "section", "settings-mode-panel");
  appendRoot(document, "div", "settings-mode-content");
  appendRoot(document, "section", "status-mode-panel");
  appendRoot(document, "div", "status-mode-content");
  appendRoot(document, "select", "inspect-pipeline-select");
  appendRoot(document, "button", "inspect-open-pipeline-btn");
  appendRoot(document, "div", "inspect-pipeline-summary");
  appendRoot(document, "p", "inspect-focus-summary");
  appendRoot(document, "div", "inspect-diagnostics-summary");
  appendRoot(document, "div", "inspect-resource-details");
  appendRoot(document, "button", "inspect-refresh-graph-btn");
  appendRoot(document, "button", "inspect-open-diagnostics-btn");
  appendRoot(document, "div", "inspect-graph-status");
  appendRoot(document, "div", "inspect-graph-container");
  appendRoot(document, "div", "workspace-mode-summary");
}
async function flushAsyncWork() {
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
}

runCheck("renderPipelines publishes selector models to the v2 owner", async () => {
  const { document } = installFakeDom();
  appendRoot(document, "div", "dashboard-v2-operate-panel");
  appendRoot(document, "div", "pipe-info-col");
  appendRoot(document, "div", "outs-col");

  const render = await loadCompiledFrontendModule("features/render.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");

  state.pipelines = [makePipeline()];
  let presented = null;

  render.configurePipelineSelectorPresentation({
    onPresentation: (model) => {
      presented = model;
    },
  });
  render.renderPipelines();

  assert.equal(presented.pipelines.length, 1);
  assert.equal(presented.pipelines[0].id, "pipe-1");
  assert.equal(document.getElementById("pipelines"), null);
  render.configurePipelineSelectorPresentation({});
});

runCheck("renderDashboardV2SettingsBody emits delegated actions without inline handlers", async () => {
  const { document } = installFakeDom();
  const container = appendRoot(
    document,
    "div",
    "dashboard-v2-settings-content",
  );

  const settings = await loadCompiledFrontendModule("features/settings.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");
  state.config = {
    serverName: "Synthetic Restream",
    transcodeProfiles: {
      mobile: {
        preset: "veryfast",
        tune: "zerolatency",
        crf: 26,
        gop: 60,
        bframes: 0,
        bitrate: 0,
        maxBitrate: 0,
        width: 854,
        height: 480,
      },
    },
  };
  settings.renderDashboardV2SettingsBody(container);
  settings.loadTranscodeProfiles();

  assert.doesNotMatch(container.innerHTML, /\son[a-z]+\s*=/i);
  assert.match(container.innerHTML, /data-settings-action="save-server-name"/);
  assert.match(container.innerHTML, /data-settings-action="reset-rate-limits"/);
  assert.match(container.innerHTML, /aria-label="Current password"/);
  assert.match(container.innerHTML, /aria-label="Global SRT ingest mode"/);
  assert.match(container.innerHTML, /id="settings-route-summary"/);
  assert.match(container.innerHTML, /Search authentication attempts/);
  assert.match(container.innerHTML, /id="auth-attempts-search-summary"/);
  assert.match(container.innerHTML, /role="status"/);
});

runCheck(
  "renderOutsColumn publishes v2 output models and preserves expansion state",
  async () => {
    const { document } = installFakeDom();
    appendRoot(document, "div", "outs-col");
    const outputList = await loadCompiledFrontendModule(
      "features/pipeline-output-list.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    state.pipelines = [
      makePipeline({
        outs: Array.from({ length: 10 }, (_, index) =>
          makeOutput({ id: `out-${index}`, name: `Output ${index}` }),
        ),
      }),
    ];
    let presented = null;

    outputList.configurePipelineOutputOverviewPresentation({
      onPresentation: (model) => {
        presented = model;
      },
    });
    outputList.renderOutsColumn("pipe-1");

    assert.equal(presented.cards.length, 8);
    assert.equal(presented.expanded, false);
    assert.equal(document.getElementById("outputs-list"), null);

    outputList.togglePipelineOutputList("pipe-1");
    assert.equal(presented.cards.length, 10);
    assert.equal(presented.expanded, true);
    assert.equal(presented.listCaption, "Showing all 10 outputs");
    outputList.configurePipelineOutputOverviewPresentation({});
  },
);

runCheck(
  "renderOutsColumn carries output control state into the v2 model",
  async () => {
    const { document } = installFakeDom();
    appendRoot(document, "div", "outs-col");

    const outputList = await loadCompiledFrontendModule(
      "features/pipeline-output-list.js",
    );
    const pipelineDeps = await loadCompiledFrontendModule(
      "features/pipeline-dependencies.js",
    );
    const controlState = await loadCompiledFrontendModule(
      "features/output-control-state.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");

    state.pipelines = [
      makePipeline({
        outs: [
          makeOutput({
            desiredState: "stopped",
            status: "off",
            rawStatus: "stopped",
            time: null,
            bitrateKbps: null,
          }),
        ],
      }),
    ];
    let presented = null;

    pipelineDeps.setPipelineViewDependencies({
      isOutputToggleBusy: () => true,
    });
    controlState.beginOutputControlIntent("pipe-1", "out-1", "starting");
    outputList.configurePipelineOutputOverviewPresentation({
      onPresentation: (model) => {
        presented = model;
      },
    });
    outputList.renderOutsColumn("pipe-1");

    assert.equal(presented.cards[0].controlLabel, "Starting...");
    assert.equal(presented.cards[0].controlDisabled, true);
    outputList.configurePipelineOutputOverviewPresentation({});
    controlState.finishOutputControlIntent("pipe-1", "out-1");
  },
);

runCheck(
  "restream process indicator reacts to lifecycle logs and health recovery",
  async () => {
    const { document } = installFakeDom();
    const badge = appendRoot(document, "div", "restream-process-indicator");
    const dot = appendRoot(document, "span", "restream-process-dot");
    const label = appendRoot(document, "span", "restream-process-text");
    badge.appendChild(dot);
    badge.appendChild(label);

    const indicator = await loadCompiledFrontendModule(
      "features/restream-process-indicator.js",
    );

    indicator.renderRestreamProcessIndicator();
    assert.equal(label.textContent, "Connecting");

    indicator.updateRestreamProcessIndicatorFromLog({
      eventType: "restream.shutdown.started",
    });
    assert.equal(label.textContent, "Stopping");

    indicator.updateRestreamProcessIndicatorFromLog({
      message: "task exited unexpectedly",
    });
    assert.equal(label.textContent, "Faulted");

    indicator.syncRestreamProcessIndicatorFromHealth("ready");
    assert.equal(label.textContent, "Running");

    indicator.syncRestreamProcessIndicatorFromHealth("degraded");
    assert.equal(label.textContent, "Degraded");
  },
);

runCheck(
  "restream process indicator keeps explicit lifecycle states ahead of API reachability hints",
  async () => {
    const { document } = installFakeDom();
    const badge = appendRoot(document, "div", "restream-process-indicator");
    const dot = appendRoot(document, "span", "restream-process-dot");
    const label = appendRoot(document, "span", "restream-process-text");
    badge.appendChild(dot);
    badge.appendChild(label);

    const indicator = await loadCompiledFrontendModule(
      "features/restream-process-indicator.js",
    );

    indicator.updateRestreamProcessIndicatorFromLog({
      eventType: "restream.shutdown.started",
    });
    assert.equal(label.textContent, "Stopping");

    indicator.syncRestreamProcessIndicatorFromApiReachability();
    assert.equal(
      label.textContent,
      "Stopping",
      "API reachability should not overwrite an explicit lifecycle state",
    );
  },
);

runCheck(
  "restream process indicator lets API reachability confirm recovery from terminal states",
  async () => {
    const { document } = installFakeDom();
    const badge = appendRoot(document, "div", "restream-process-indicator");
    const dot = appendRoot(document, "span", "restream-process-dot");
    const label = appendRoot(document, "span", "restream-process-text");
    badge.appendChild(dot);
    badge.appendChild(label);

    const indicator = await loadCompiledFrontendModule(
      "features/restream-process-indicator.js",
    );

    indicator.updateRestreamProcessIndicatorFromLog({
      eventType: "restream.shutdown.completed",
    });
    assert.equal(label.textContent, "Stopped");

    indicator.syncRestreamProcessIndicatorFromApiReachability();
    assert.equal(
      label.textContent,
      "Running",
      "reachable API telemetry should revive a previously stopped process indicator",
    );

    indicator.updateRestreamProcessIndicatorFromLog({
      message: "task exited unexpectedly",
    });
    assert.equal(label.textContent, "Faulted");

    indicator.syncRestreamProcessIndicatorFromApiReachability();
    assert.equal(
      label.textContent,
      "Running",
      "reachable API telemetry should also clear a stale fault once the process is back",
    );

    indicator.updateRestreamProcessIndicatorFromLog({
      eventType: "restream.shutdown.started",
    });
    assert.equal(label.textContent, "Stopping");

    indicator.syncRestreamProcessIndicatorFromApiReachability();
    assert.equal(
      label.textContent,
      "Stopping",
      "shutdown-in-progress should stay ahead of plain API reachability hints",
    );
  },
);

runCheck(
  "renderPipelineInfoColumn publishes input status models across refreshes",
  async () => {
    const { document } = installFakeDom();
    appendRoot(document, "div", "pipe-info-col");

    const pipelineView = await loadCompiledFrontendModule(
      "features/pipeline-view/index.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    const inputModels = [];
    pipelineView.configurePipelineInputStatusPresentation({
      onPresentation: (model) => {
        inputModels.push(model);
      },
    });

    state.pipelines = [
      makePipeline({
        input: {
          ...makePipeline().input,
          publisher: { protocol: "srt", remoteAddr: "10.0.0.1:5000" },
        },
        hlsPreview: {
          active: true,
          persistentConsumers: 1,
          lastAccessAgeMs: 2000,
          segments: 3,
          playlistBytes: 256,
        },
      }),
    ];

    pipelineView.renderPipelineInfoColumn("pipe-1");
    const firstModel = inputModels.at(-1);

    assert.equal(firstModel.publisherLabel, "SRT");
    assert.equal(firstModel.publisherDetail, "10.0.0.1:5000");
    assert.equal(firstModel.publisherHealth.label, "Healthy");
    assert.equal(document.getElementById("publisher-meta"), null);

    state.pipelines[0].input.time = 35_000;
    state.pipelines[0].hlsPreview.lastAccessAgeMs = 5_000;
    pipelineView.renderPipelineInfoColumn("pipe-1");
    const secondModel = inputModels.at(-1);

    assert.equal(secondModel.uptimeLabel, "0:00:35 uptime");
    assert.equal(secondModel.previewDetail, "3 segments · 1 viewer");
    pipelineView.configurePipelineInputStatusPresentation({});
  },
);

runCheck(
  "renderPipelineInfoColumn shows file ingest controls for file sources",
  async () => {
    const { document } = installFakeDom();
    appendRoot(document, "div", "pipe-info-col");

    const pipelineView = await loadCompiledFrontendModule(
      "features/pipeline-view/index.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
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

    state.pipelines = [
      makePipeline({
        inputSource: "file:session-recording.ts",
        input: {
          ...makePipeline().input,
          status: "off",
        },
        fileIngest: {
          configured: true,
          id: "ingest-1",
          filename: "session-recording.ts",
          running: false,
        },
        ingestUrls: {
          rtmp: "rtmp://example.com/live/secret",
          srt: "srt://example.com:9000?streamid=secret",
        },
      }),
    ];

    pipelineView.renderPipelineInfoColumn("pipe-1");

    const inputStatus = inputModels.at(-1);
    const details = new Map(
      inputStatus.fileSource.details.map((detail) => [
        detail.key,
        detail.value,
      ]),
    );
    assert.equal(headerModels.at(-1).fileIngestControl?.label, "Start File");
    assert.equal(inputStatus.fileSource.filename, "session-recording.ts");
    assert.equal(details.get("container"), "MPEG-TS");
    assert.equal(details.get("loop"), "Disabled");
    assert.equal(details.get("start"), "00:00:00");
    assert.equal(inputStatus.liveSource, null);
    assert.equal(document.getElementById("file-source-section"), null);
    assert.equal(document.getElementById("stream-key-section"), null);
    pipelineView.configurePipelineInputStatusPresentation({});
  },
);

runCheck(
  "media library search matches filename, converted file, and status",
  async () => {
    const mediaLibrary = await loadCompiledFrontendModule(
      "features/media-library.js",
    );
    const file = {
      name: "festival-recording.ts",
      kind: "recording",
      sourceName: "festival-recording.ts",
      convertedName: "festival-recording.mp4",
      playName: "festival-recording.mp4",
      conversionStatus: "ready",
      size: 1200,
      modifiedAt: "2026-07-15T00:00:00Z",
    };

    assert.equal(mediaLibrary.mediaFileMatchesSearch(file, ""), true);
    assert.equal(mediaLibrary.mediaFileMatchesSearch(file, "festival"), true);
    assert.equal(mediaLibrary.mediaFileMatchesSearch(file, "mp4"), true);
    assert.equal(mediaLibrary.mediaFileMatchesSearch(file, "ready"), true);
    assert.equal(mediaLibrary.mediaFileMatchesSearch(file, "source"), false);
  },
);

runCheck(
  "renderPipelineInfoColumn keeps the active file-source panel ahead of stale async loads",
  async () => {
    const { document, window } = installFakeDom();
    if (!FakeElement.prototype.pause) {
      FakeElement.prototype.pause = () => {};
    }
    if (!FakeElement.prototype.load) {
      FakeElement.prototype.load = () => {};
    }
    window.location.href = "http://localhost/?mode=pipeline&p=pipe-1";
    appendRoot(document, "div", "pipe-info-col");

    const requests = [];
    let resolveMediaList;
    let resolveAlphaAnalysis;
    let resolveBetaAnalysis;
    const mediaListReady = new Promise((resolve) => {
      resolveMediaList = resolve;
    });
    const alphaAnalysisReady = new Promise((resolve) => {
      resolveAlphaAnalysis = resolve;
    });
    const betaAnalysisReady = new Promise((resolve) => {
      resolveBetaAnalysis = resolve;
    });

    globalThis.fetch = async (url) => {
      const href = String(url);
      requests.push(href);

      if (href === "/api/v1/media") {
        await mediaListReady;
        return new Response(
          JSON.stringify({
            files: [
              {
                name: "alpha.ts",
                kind: "recording",
                size: 1200,
                modifiedAt: "2026-06-30T00:00:00Z",
              },
              {
                name: "beta.ts",
                kind: "recording",
                size: 3400,
                modifiedAt: "2026-06-30T00:05:00Z",
              },
            ],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }

      if (href === "/api/v1/media/alpha.ts/analysis") {
        await alphaAnalysisReady;
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

      if (href === "/api/v1/media/beta.ts/analysis") {
        await betaAnalysisReady;
        return new Response(
          JSON.stringify({
            videoCodec: "hevc",
            fps: 60,
            durationSec: 120,
            averageKeyframeIntervalSec: 1,
            maxKeyframeIntervalSec: 1,
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }

      throw new Error(`Unexpected fetch: ${href}`);
    };

    const pipelineView = await loadCompiledFrontendModule(
      "features/pipeline-view/index.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    const inputModels = [];
    pipelineView.configurePipelineInputStatusPresentation({
      onPresentation: (model) => {
        inputModels.push(model);
      },
    });

    state.pipelines = [
      makePipeline({
        inputSource: "file:alpha.ts",
        fileIngest: {
          configured: true,
          id: "ingest-1",
          filename: "alpha.ts",
          running: false,
        },
        ingestUrls: {
          rtmp: "rtmp://example.com/live/alpha",
          srt: "srt://example.com:9000?streamid=alpha",
        },
      }),
      makePipeline({
        id: "pipe-2",
        name: "Pipeline 2",
        key: "stream-key-2",
        inputSource: "file:beta.ts",
        fileIngest: {
          configured: true,
          id: "ingest-2",
          filename: "beta.ts",
          running: false,
        },
        outs: [makeOutput({ pipe: "pipe-2" })],
        ingestUrls: {
          rtmp: "rtmp://example.com/live/beta",
          srt: "srt://example.com:9000?streamid=beta",
        },
      }),
    ];

    pipelineView.renderPipelineInfoColumn("pipe-1");
    assert.equal(inputModels.at(-1).fileSource.filename, "alpha.ts");

    window.location.href = "http://localhost/?mode=pipeline&p=pipe-2";
    pipelineView.renderPipelineInfoColumn("pipe-2");
    assert.equal(inputModels.at(-1).fileSource.filename, "beta.ts");
    assert.equal(
      requests.includes("/api/v1/media/beta.ts/analysis"),
      true,
      "switching to a different file-backed pipeline should start its analysis immediately",
    );

    resolveAlphaAnalysis();
    await flushAsyncWork();
    assert.equal(
      inputModels.at(-1).fileSource.filename,
      "beta.ts",
      "a stale alpha analysis completion should not repaint the pipe-2 panel",
    );

    resolveMediaList();
    await flushAsyncWork();
    const afterMediaListDetails = new Map(
      inputModels.at(-1).fileSource.details.map((detail) => [
        detail.key,
        detail.value,
      ]),
    );
    assert.equal(
      afterMediaListDetails.get("size"),
      "3.3 KiB",
      "shared media metadata should re-render the currently selected pipeline",
    );

    resolveBetaAnalysis();
    await flushAsyncWork();
    const afterAnalysisDetails = new Map(
      inputModels.at(-1).fileSource.details.map((detail) => [
        detail.key,
        detail.value,
      ]),
    );
    assert.equal(
      afterAnalysisDetails.get("codec"),
      "HEVC",
    );
    pipelineView.configurePipelineInputStatusPresentation({});
  },
);

runCheck(
  "renderPipelineInfoColumn fills live video and audio stat surfaces",
  async () => {
    const { document } = installFakeDom();
    appendRoot(document, "div", "pipe-info-col");

    const pipelineView = await loadCompiledFrontendModule(
      "features/pipeline-view/index.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    const inputModels = [];
    pipelineView.configurePipelineInputStatusPresentation({
      onPresentation: (model) => {
        inputModels.push(model);
      },
    });

    state.pipelines = [
      makePipeline({
        input: {
          ...makePipeline().input,
          status: "on",
          time: 42_000,
          video: {
            codec: "h264",
            width: 1920,
            height: 1080,
            fps: 60,
            level: "4.2",
            profile: "High",
            pid: 256,
          },
          videoTrackSelection: {
            mode: "firstVideoOnly",
            selectedTrackIndex: 0,
            availableTrackCount: 2,
            ignoredTrackCount: 1,
          },
          audioTracks: [
            {
              index: 0,
              pid: 257,
              codec: "aac",
              channels: 2,
              sample_rate: 48_000,
              language: "eng",
              title: "Main Mix",
              profile: "LC",
            },
          ],
        },
        stats: {
          inputBitrateKbps: 4500,
          outputBitrateKbps: 2200,
          readerCount: 3,
          outputCount: 1,
          readerMismatch: false,
          unexpectedReadersCount: 0,
        },
        ingestUrls: {
          rtmp: "rtmp://example.com/live/stream-key",
          srt: "srt://example.com:10080?streamid=publish:stream-key",
        },
      }),
    ];

    pipelineView.renderPipelineInfoColumn("pipe-1");

    const inputStatus = inputModels.at(-1);
    const traffic = inputStatus.metricGroups.find(
      ({ key }) => key === "traffic",
    );
    const video = inputStatus.metricGroups.find(({ key }) => key === "video");
    const videoMetrics = new Map(
      video.metrics.map((metric) => [metric.key, metric.value]),
    );
    const trafficMetrics = new Map(
      traffic.metrics.map((metric) => [metric.key, metric.value]),
    );
    assert.equal(videoMetrics.get("codec"), "H264");
    assert.equal(videoMetrics.get("resolution"), "1920×1080");
    assert.equal(videoMetrics.get("pid"), "0x100");
    assert.equal(videoMetrics.get("selection"), "Track 1 of 2");
    assert.equal(inputStatus.audioTracks[0].label, "Main Mix");
    assert.equal(inputStatus.audioTracks[0].channels, "Stereo (2 ch)");
    assert.equal(trafficMetrics.get("readers"), "3");
    assert.equal(trafficMetrics.get("outputs"), "1");
    pipelineView.configurePipelineInputStatusPresentation({});
  },
);

runCheck(
  "renderPipelineInfoColumn publishes complete long audio-track lists",
  async () => {
    const { document } = installFakeDom();
    appendRoot(document, "div", "pipe-info-col");

    const pipelineView = await loadCompiledFrontendModule(
      "features/pipeline-view/index.js",
    );
    const { state } = await loadCompiledFrontendModule("core/state.js");
    const inputModels = [];
    pipelineView.configurePipelineInputStatusPresentation({
      onPresentation: (model) => {
        inputModels.push(model);
      },
    });
    state.pipelines = [
      makePipeline({
        input: {
          ...makePipeline().input,
          status: "on",
          audioTracks: Array.from({ length: 10 }, (_, index) => ({
            index,
            pid: 257 + index,
            codec: "aac",
            channels: 2,
            sample_rate: 48_000,
            language: "und",
            profile: "LC",
          })),
        },
      }),
    ];

    pipelineView.renderPipelineInfoColumn("pipe-1");

    assert.equal(inputModels.at(-1).audioTracks.length, 10);
    assert.equal(inputModels.at(-1).audioLabel, "10 audio tracks");
    assert.equal(document.getElementById("input-audio-tracks"), null);
    pipelineView.configurePipelineInputStatusPresentation({});
  },
);

runCheck("inspect summary keeps retry badges non-wrapping", async () => {
  const { document, window } = installFakeDom();
  appendInspectDom(document);
  window.location.href = "http://localhost/?mode=inspect&p=pipe-1";
  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};

  const fetchCalls = [];
  globalThis.fetch = async (url) => {
    fetchCalls.push(String(url));
    return {
      ok: true,
      status: 200,
      async json() {
        return { pipelineId: "pipe-1", nodes: [], edges: [] };
      },
    };
  };

  const modes = await loadCompiledFrontendModule("app/modes.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");

  state.pipelines = [
    makePipeline({
      outs: [
        makeOutput({
          desiredState: "running",
          status: "retrying",
          retrying: true,
        }),
      ],
    }),
  ];

  modes.renderDashboardModes();
  await flushAsyncWork();

  const summaryHtml = document.getElementById(
    "inspect-pipeline-summary",
  ).innerHTML;
  assert.match(summaryHtml, /Output retrying/);
  assert.match(summaryHtml, /shrink-0/);
  assert.match(summaryHtml, /whitespace-nowrap/);
  assert.equal(
    document.getElementById("inspect-focus-summary").textContent,
    "Inspection focus · 1 blocker before active probes · 1 fault candidate · Inspect recent errors and retry backoff before forcing a restart.",
  );
  assert.equal(fetchCalls.length >= 1, true);
});

runCheck("inspect summary escapes redacted output URLs", async () => {
  const { document, window } = installFakeDom();
  appendInspectDom(document);
  window.location.href = "http://localhost/?mode=inspect&p=pipe-1";
  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};
  globalThis.fetch = async () => ({
    ok: true,
    status: 200,
    async json() {
      return { pipelineId: "pipe-1", nodes: [], edges: [] };
    },
  });

  const modes = await loadCompiledFrontendModule("app/modes.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");

  state.pipelines = [
    makePipeline({
      outs: [
        makeOutput({
          url: 'rtmp://example.com/live/abcdefghijklmnopqrstuvwxyz"><img src=x onerror=alert(1)>',
        }),
      ],
    }),
  ];

  modes.renderDashboardModes();
  await flushAsyncWork();

  const summaryHtml = document.getElementById(
    "inspect-pipeline-summary",
  ).innerHTML;
  assert.doesNotMatch(summaryHtml, /<img/i);
  assert.match(summaryHtml, /&lt;img/);
  assert.match(summaryHtml, /\*\*\*/);
  assert.doesNotMatch(summaryHtml, /abcdefghijklmnopqrstuvwxyz/);
});

runCheck("inspect graph refreshes when pipeline state changes", async () => {
  const { document, window } = installFakeDom();
  appendInspectDom(document);
  window.location.href = "http://localhost/?mode=inspect&p=pipe-1";
  globalThis.setInterval = () => 1;
  globalThis.clearInterval = () => {};

  const fetchCalls = [];
  globalThis.fetch = async (url) => {
    fetchCalls.push(String(url));
    return {
      ok: true,
      status: 200,
      async json() {
        return { pipelineId: "pipe-1", nodes: [], edges: [] };
      },
    };
  };

  const modes = await loadCompiledFrontendModule("app/modes.js");
  const { state } = await loadCompiledFrontendModule("core/state.js");

  state.pipelines = [makePipeline()];
  modes.renderDashboardModes();
  await flushAsyncWork();
  const firstFetchCount = fetchCalls.length;

  state.pipelines[0].outs[0].status = "retrying";
  state.pipelines[0].outs[0].retrying = true;
  modes.renderDashboardModes();
  await flushAsyncWork();

  assert.equal(fetchCalls.length > firstFetchCount, true);
});

runCheck("metric-format reuses subtle-unit spans across updates", async () => {
  const { document } = installFakeDom();
  const metric = appendRoot(document, "div", "metric");

  const metricFormat = await loadCompiledFrontendModule(
    "features/metric-format.js",
  );

  metricFormat.setBitrateWithSubtleUnit("metric", 1500);
  const firstValueSpan = metric.children[0];
  const firstUnitSpan = metric.children[1];
  const firstAppendCount = metric.stats.appendChildCalls;

  metricFormat.setBitrateWithSubtleUnit("metric", 2750);

  assert.equal(metric.children[0], firstValueSpan);
  assert.equal(metric.children[1], firstUnitSpan);
  assert.equal(metric.stats.appendChildCalls, firstAppendCount);
  assert.equal(metric.textContent, "2.8Mb/s");
});

runCheck(
  "renderDashboardModes skips overview work when pipeline mode is active",
  async () => {
    const { document, window } = installFakeDom();
    window.location.href = "http://localhost/?mode=pipeline";
    appendRoot(document, "div", "overview-mode-content");
    appendRoot(document, "div", "dashboard-v2-operate-panel");

    const modes = await loadCompiledFrontendModule("app/modes.js");
    const { state } = await loadCompiledFrontendModule("core/state.js");

    state.pipelines = [makePipeline()];
    modes.renderDashboardModes();

    const overview = document.getElementById("overview-mode-content");
    assert.ok(overview instanceof FakeElement);
    assert.equal(overview.stats.innerHTMLWrites, 0);
  },
);

runCheck(
  "renderDashboardModes does not refetch media mode data on same-mode rerenders",
  async () => {
    const { document, window } = installFakeDom();
    window.location.href = "http://localhost/?mode=media";
    appendRoot(document, "div", "dashboard-v2-operate-panel");
    appendDashboardV2Roots(document);
    appendRoot(document, "div", "overview-mode-panel");
    appendRoot(document, "div", "inspect-mode-panel");
    appendRoot(document, "div", "control-mode-panel");
    appendRoot(document, "div", "media-mode-panel");
    appendRoot(document, "div", "settings-mode-panel");
    appendRoot(document, "div", "status-mode-panel");
    appendRoot(document, "div", "media-mode-content");

    const requests = [];
    globalThis.fetch = async (url) => {
      const href = String(url);
      requests.push(href);

      if (href === "/api/v1/settings?jobs=latest") {
        return new Response(
          JSON.stringify({
            serverName: "Restream",
            pipelines: [],
            outputs: [],
            jobs: [],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      if (href === "/api/v1/engine/health") {
        return new Response(
          JSON.stringify({ status: "ready", pipelines: {} }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      if (href === "/metrics/system") {
        return new Response(
          JSON.stringify({
            generatedAt: "2026-06-30T00:00:00Z",
            mediaDisk: {
              usedBytes: 100,
              totalBytes: 200,
              usedPercent: 50,
              mountPoint: "/media",
              mediaRoot: "/srv/media",
            },
            network: { downloadKbps: 1, uploadKbps: 2, interfaces: [] },
            disk: { usedPercent: 40 },
            cpu: { usagePercent: 12 },
            memory: { usedPercent: 20 },
            engine: {
              cpuPercent: 3,
              totalMemoryBytes: 1234,
              cpuSampleReady: true,
            },
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      if (href === "/api/v1/media") {
        return new Response(
          JSON.stringify({
            files: [
              {
                name: "recording-1.ts",
                size: 1024,
                modifiedAt: "2026-06-30T00:00:00Z",
                kind: "recording",
              },
            ],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }

      throw new Error(`Unexpected fetch: ${href}`);
    };

    const modes = await loadCompiledFrontendModule("app/modes.js");

    modes.renderDashboardModes();
    await flushAsyncWork();
    await flushAsyncWork();

    assert.equal(
      requests.filter((href) => href === "/api/v1/settings?view=dashboard")
        .length,
      0,
    );
    assert.equal(
      requests.filter((href) => href === "/api/v1/engine/health").length,
      0,
    );
    assert.equal(
      requests.filter((href) => href === "/metrics/system").length,
      1,
    );
    assert.equal(requests.filter((href) => href === "/api/v1/media").length, 1);

    requests.length = 0;
    modes.renderDashboardModes();
    await flushAsyncWork();

    assert.deepEqual(
      requests,
      [],
      "same-mode rerender should not refetch runtime or media inventory",
    );
  },
);

runCheck(
  "renderDashboardModes upgrades dashboard config to full settings on settings mode entry",
  async () => {
    const { document, window } = installFakeDom();
    window.location.href = "http://localhost/?mode=settings";
    appendRoot(document, "div", "overview-mode-panel");
    appendRoot(document, "div", "dashboard-v2-operate-panel");
    appendDashboardV2Roots(document);
    appendRoot(document, "div", "inspect-mode-panel");
    appendRoot(document, "div", "control-mode-panel");
    appendRoot(document, "div", "media-mode-panel");
    appendRoot(document, "div", "settings-mode-panel");
    appendRoot(document, "div", "status-mode-panel");
    appendRoot(document, "div", "settings-mode-content");

    const requests = [];
    globalThis.fetch = async (url) => {
      const href = String(url);
      requests.push(href);

      if (href === "/api/v1/settings") {
        return new Response(
          JSON.stringify({
            serverName: "Restream",
            ingestHost: "stream.example.com",
            ingestSecurity: {
              failureLimit: 10,
              failureWindowMs: 60000,
              banMs: 600000,
              trackedIpLimit: 10000,
            },
            recordingSettings: {
              retainSourceTs: true,
            },
            srtIngest: {
              mode: "encrypted",
              passphrase: "supersecret1",
              pbkeylen: 24,
            },
            transcodeProfiles: {
              custom: {
                preset: "fast",
                tune: "zerolatency",
                crf: 23,
                gop: 2,
                bframes: 0,
                bitrate: 2500,
                maxBitrate: 3000,
                width: 1280,
                height: 720,
              },
            },
            pipelines: [],
            outputs: [],
            jobs: [],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }
      if (href === "/api/v1/security/rate-limits") {
        return new Response(
          JSON.stringify({
            attempts: [
              {
                scope: "dashboard-login",
                ip: "203.0.113.10",
                failureCount: 2,
                banned: false,
                banRemainingMs: null,
              },
            ],
          }),
          { status: 200, headers: { "content-type": "application/json" } },
        );
      }

      throw new Error(`Unexpected fetch: ${href}`);
    };

    const { state } = await loadCompiledFrontendModule("core/state.js");
    const modes = await loadCompiledFrontendModule("app/modes.js");
    state.config = {
      serverName: "Restream",
      ingestHost: "stream.example.com",
      transcodeProfiles: {},
      pipelines: [],
      outputs: [],
      jobs: [],
    };

    modes.renderDashboardModes();
    await flushAsyncWork();
    await flushAsyncWork();

    assert.deepEqual(requests, [
      "/api/v1/settings",
      "/api/v1/security/rate-limits",
    ]);
    assert.equal(state.config.ingestSecurity?.failureLimit, 10);
    assert.equal(state.config.recordingSettings?.retainSourceTs, true);
    assert.equal(state.config.srtIngest?.pbkeylen, 24);
    assert.equal(state.config.transcodeProfiles?.custom?.preset, "fast");
  },
);

runCheck(
  "initDashboardApp wires dashboard mode bootstrapping once",
  async () => {
    const { document, window } = installFakeDom();
    window.location.href = "http://localhost/?mode=pipeline";
    appendRoot(document, "div", "dashboard-v2-operate-panel");
    appendDashboardV2Roots(document);
    const v2Loader = await loadCompiledFrontendModule("app/dashboard-v2-loader.js");
    v2Loader.setDashboardV2PresentationScope({
      overviewActive: false,
      pipelineActive: false,
    });
    const app = await loadCompiledFrontendModule("app/dashboard-app.js");
    const deps = await loadCompiledFrontendModule(
      "features/pipeline-dependencies.js",
    );

    app.initDashboardApp();
    const firstSetDashboardMode = window.setDashboardMode;
    app.initDashboardApp();

    assert.equal(typeof firstSetDashboardMode, "function");
    assert.equal(window.setDashboardMode, firstSetDashboardMode);
    await flushAsyncWork();
    assert.equal(
      typeof deps.pipelineViewDependencies.refreshDashboardRuntime,
      "function",
    );
  },
);
