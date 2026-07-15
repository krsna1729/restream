import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

function output(overrides = {}) {
  return {
    id: "out-1",
    name: "Primary output",
    url: "rtmp://example/live",
    desiredState: "started",
    status: "running",
    phase: "sending",
    failurePhase: null,
    lastError: null,
    lastProgressAgeMs: null,
    retrying: false,
    retryAttempts: null,
    retryRemainingMs: null,
    flapping: false,
    recentFailureCount: 0,
    bitrateKbps: 1500,
    time: 420_000,
    ...overrides,
  };
}

function pipeline({
  id,
  name = id,
  inputStatus = "on",
  outputs = [],
  inputKbps = 0,
  outputKbps = 0,
  publisherProtocol = null,
  recordingEnabled = false,
  recordingActive = false,
  inputSource = null,
  streamKey = `${id}-key`,
  ingestUrls = { rtmp: null, srt: null },
  fileIngest = null,
  inputOverrides = {},
  hlsPreview = {},
}) {
  return {
    id,
    name,
    key: streamKey,
    ingestUrls,
    input: {
      status: inputStatus,
      probeReady: true,
      flapping: false,
      disconnectGraceActive: false,
      recentDisconnectError: false,
      lastDisconnectReason: null,
      probePendingMs: null,
      time: null,
      video: null,
      audioTracks: [],
      unexpectedReadersCount: 0,
      recentDisconnectCount: 0,
      publisher: publisherProtocol ? { protocol: publisherProtocol } : null,
      ...inputOverrides,
    },
    outs: outputs,
    stats: {
      inputBitrateKbps: inputKbps,
      outputBitrateKbps: outputKbps,
      readerCount: inputOverrides.readers ?? 0,
      outputCount: outputs.length,
    },
    recording: { enabled: recordingEnabled, active: recordingActive },
    inputSource,
    fileIngest,
    hlsPreview: {
      active: false,
      persistentConsumers: 0,
      lastAccessAgeMs: null,
      segments: 0,
      playlistBytes: 0,
      ...hlsPreview,
    },
  };
}

test("pipeline selector model sorts rows and preserves one valid selection", async () => {
  const { buildPipelineOperateSelectorModel } =
    await loadCompiledFrontendModule("features/pipeline-operate-view-model.js");
  const model = buildPipelineOperateSelectorModel(
    [
      pipeline({
        id: "retrying",
        name: "Zulu retry",
        outputs: [
          { status: "retrying", desiredState: "started", retrying: true },
        ],
        inputKbps: 2400,
      }),
      pipeline({
        id: "healthy",
        name: "Alpha healthy",
        outputs: [{ status: "running", desiredState: "started" }],
        inputKbps: 3200,
        outputKbps: 2900,
      }),
    ],
    "retrying",
  );

  assert.equal(model.selectedPipelineId, "retrying");
  assert.deepEqual(model.pipelines, [
    {
      id: "healthy",
      name: "Alpha healthy",
      selected: false,
      statusLabel: "Live",
      statusTone: "success",
      inputRate: "3.2 Mb/s",
      outputRate: "2.9 Mb/s",
      runningOutputs: 1,
      totalOutputs: 1,
    },
    {
      id: "retrying",
      name: "Zulu retry",
      selected: true,
      statusLabel: "Output retrying",
      statusTone: "warning",
      inputRate: "2.4 Mb/s",
      outputRate: "0 Kb/s",
      runningOutputs: 0,
      totalOutputs: 1,
    },
  ]);
});

test("pipeline selector model drops stale selection ids", async () => {
  const { buildPipelineOperateSelectorModel } =
    await loadCompiledFrontendModule("features/pipeline-operate-view-model.js");
  const model = buildPipelineOperateSelectorModel(
    [pipeline({ id: "healthy" })],
    "removed",
  );

  assert.equal(model.selectedPipelineId, null);
  assert.equal(model.pipelines[0].selected, false);
});

test("pipeline header model derives identity, status, and action availability", async () => {
  const { buildPipelineOperateHeaderModel } = await loadCompiledFrontendModule(
    "features/pipeline-operate-view-model.js",
  );
  const active = buildPipelineOperateHeaderModel(
    [
      pipeline({
        id: "live",
        streamKey: "live-stream-key-that-is-long-12345",
        ingestUrls: {
          rtmp:
            "rtmp://ingest.example/live/live-stream-key-that-is-long-12345",
          srt: "srt://ingest.example:9000?streamid=live-stream-key-that-is-long-12345",
        },
        name: "Live program",
        publisherProtocol: "rtmp",
        outputs: [{ status: "running", desiredState: "started" }],
        inputKbps: 3200,
        outputKbps: 2900,
      }),
    ],
    "live",
  );

  assert.deepEqual(active, {
    id: "live",
    name: "Live program",
    health: { label: "Live", tone: "success", detail: "healthy" },
    sourceLabel: "RTMP",
    inputRate: "3.2 Mb/s",
    outputRate: "2.9 Mb/s",
    outputsLabel: "1/1 outputs",
    recordingLabel: "Recording off",
    canDiagnose: true,
    diagnoseDisabledReason: undefined,
    canEdit: true,
    editDisabledReason: undefined,
    recordingControl: {
      label: "Record",
      disabled: false,
      title: "",
      danger: false,
      outlined: true,
    },
    fileIngestControl: null,
  });

  const recording = buildPipelineOperateHeaderModel(
    [
      pipeline({
        id: "file",
        inputStatus: "off",
        inputSource: "file:clip.mp4",
        fileIngest: {
          configured: true,
          id: "ingest-1",
          filename: "clip.mp4",
          running: false,
        },
        recordingEnabled: true,
        recordingActive: true,
      }),
    ],
    "file",
    { recordingIntent: "stopping", fileIngestIntent: "starting" },
  );
  assert.equal(recording.sourceLabel, "File · clip.mp4");
  assert.equal(recording.recordingLabel, "Recording active");
  assert.equal(recording.canDiagnose, false);
  assert.equal(recording.canEdit, false);
  assert.equal(
    recording.diagnoseDisabledReason,
    "Input must be online to run diagnostics",
  );
  assert.equal(recording.editDisabledReason, "Stop recording before editing");
  assert.deepEqual(recording.recordingControl, {
    label: "Stopping...",
    disabled: true,
    title: "",
    danger: true,
    outlined: false,
  });
  assert.deepEqual(recording.fileIngestControl, {
    label: "Starting File...",
    disabled: true,
    title: "Start file ingest for clip.mp4",
    danger: false,
    outlined: false,
  });
  assert.equal(buildPipelineOperateHeaderModel([], "removed"), null);
});

test("pipeline input status model projects publisher, preview, and media state", async () => {
  const { buildPipelineOperateInputStatusModel } =
    await loadCompiledFrontendModule("features/pipeline-operate-view-model.js");
  const live = buildPipelineOperateInputStatusModel(
    [
      pipeline({
        id: "live",
        streamKey: "live-stream-key-that-is-long-12345",
        ingestUrls: {
          rtmp:
            "rtmp://ingest.example/live/live-stream-key-that-is-long-12345",
          srt: "srt://ingest.example:9000?streamid=live-stream-key-that-is-long-12345",
        },
        inputKbps: 2000,
        outputKbps: 1500,
        outputs: [output({ id: "out-live" })],
        publisherProtocol: "rtmp",
        inputOverrides: {
          time: 3_723_000,
          publisher: {
            protocol: "rtmp",
            remoteAddr: "203.0.113.7:4242",
            quality: {},
          },
          video: {
            codec: "h264",
            width: 1920,
            height: 1080,
            fps: 30,
            profile: "High",
            level: "4.1",
            pid: 256,
          },
          videoTrackSelection: {
            mode: "firstVideoOnly",
            selectedTrackIndex: 0,
            availableTrackCount: 2,
            ignoredTrackCount: 1,
          },
          audioTracks: [{ index: 0, codec: "aac" }],
          readers: 2,
        },
        hlsPreview: { active: true, segments: 4, persistentConsumers: 1 },
      }),
    ],
    "live",
  );

  assert.deepEqual(live, {
    id: "live",
    status: { label: "Live input", tone: "success", detail: "Receiving media" },
    uptimeLabel: "1:02:03 uptime",
    publisherLabel: "RTMP",
    publisherDetail: "203.0.113.7:4242",
    publisherHealth: {
      label: "Healthy",
      tone: "success",
      detail: "Publisher link",
    },
    preview: {
      label: "Preview live",
      tone: "success",
      detail: "HLS segmenter active",
    },
    previewDetail: "4 segments · 1 viewer",
    previewEnabled: true,
    previewKeyAssigned: true,
    videoLabel: "H264 · 1920×1080 · 30 fps",
    audioLabel: "1 audio track",
    unexpectedReadersLabel: null,
    metricGroups: [
      {
        key: "traffic",
        label: "Traffic",
        metrics: [
          { key: "input-rate", label: "Input bitrate", value: "2.0 Mb/s" },
          { key: "output-rate", label: "Output bitrate", value: "1.5 Mb/s" },
          { key: "readers", label: "Readers", value: "2" },
          { key: "outputs", label: "Outputs", value: "1" },
        ],
      },
      {
        key: "video",
        label: "Video",
        metrics: [
          { key: "codec", label: "Codec", value: "H264" },
          { key: "resolution", label: "Resolution", value: "1920×1080" },
          { key: "fps", label: "FPS", value: "30" },
          { key: "profile", label: "Profile", value: "High" },
          { key: "level", label: "Level", value: "4.1" },
          { key: "pid", label: "PID", value: "0x100" },
          {
            key: "selection",
            label: "Track selection",
            value: "Track 1 of 2",
          },
        ],
      },
    ],
    liveSource: {
      pipelineId: "live",
      streamKeyLabel: "live-stream-key-that***12345",
      protocols: [
        {
          id: "rtmp",
          label: "RTMP",
          selected: true,
          urlLabel:
            "rtmp://ingest.example/live/live-stream-key-that***12345",
        },
        {
          id: "srt",
          label: "SRT",
          selected: false,
          urlLabel:
            "srt://ingest.example:9000?streamid=live-stream-key-that***12345",
        },
      ],
    },
    fileSource: null,
    audioTracks: [],
  });

  const offline = buildPipelineOperateInputStatusModel(
    [
      pipeline({
        id: "offline",
        inputStatus: "off",
        inputOverrides: {
          recentDisconnectError: true,
          lastDisconnectReason: "connection reset",
          unexpectedReadersCount: 2,
        },
      }),
    ],
    "offline",
  );
  assert.equal(offline.status.label, "Input offline");
  assert.equal(offline.status.tone, "error");
  assert.equal(offline.preview.label, "Preview unavailable");
  assert.equal(offline.previewEnabled, false);
  assert.equal(offline.previewKeyAssigned, true);
  assert.equal(offline.unexpectedReadersLabel, "2 unexpected readers");
  assert.deepEqual(offline.metricGroups, []);
  assert.equal(buildPipelineOperateInputStatusModel([], "removed"), null);
});

test("pipeline output overview projects rollup and prioritized attention", async () => {
  const { buildPipelineOutputOverviewModel } = await loadCompiledFrontendModule(
    "features/pipeline-operate-view-model.js",
  );
  const model = buildPipelineOutputOverviewModel(
    [
      pipeline({
        id: "live",
        outputs: [
          output(),
          output({
            id: "retrying",
            name: "Retry destination",
            status: "retrying",
            phase: "connect",
            retrying: true,
            retryAttempts: 3,
            retryRemainingMs: 6000,
            lastError: "connection refused",
            bitrateKbps: null,
            monitoringUrl: "https://monitor.example.invalid/retrying",
          }),
          output({
            id: "stopped",
            name: "Archive",
            desiredState: "stopped",
            status: "off",
            phase: "idle",
            bitrateKbps: null,
          }),
        ],
      }),
    ],
    "live",
    [{ outputId: "retrying", intent: null, busy: false }],
  );

  assert.deepEqual(model, {
    pipelineId: "live",
    activeLabel: "1/3 active",
    aggregateRate: "1.5 Mb/s",
    counts: [
      { key: "retrying", label: "Retrying", tone: "warning", count: 1 },
      { key: "running", label: "Running", tone: "success", count: 1 },
      { key: "stopped", label: "Stopped", tone: "neutral", count: 1 },
    ],
    attention: [
      {
        id: "retrying",
        name: "Retry destination",
        status: { label: "Retrying", tone: "warning", detail: "Retry in 6s" },
        encodingLabel: "source",
        rateLabel: "--",
      },
    ],
    cards: [
      {
        id: "out-1",
        name: "Primary output",
        urlLabel: "rtmp://example/live",
        status: {
          label: "Running",
          tone: "success",
          detail: "Delivering media",
        },
        encodingLabel: "source",
        rateLabel: "1.5 Mb/s",
        uptimeLabel: "0:07:00",
        controlLabel: "Stop",
        controlDisabled: false,
        monitorAvailable: false,
        deleteDisabled: true,
      },
      {
        id: "retrying",
        name: "Retry destination",
        urlLabel: "rtmp://example/live",
        status: {
          label: "Retrying",
          tone: "warning",
          detail: "Retry in 6s",
        },
        encodingLabel: "source",
        rateLabel: "--",
        uptimeLabel: null,
        controlLabel: "Stop",
        controlDisabled: false,
        monitorAvailable: true,
        deleteDisabled: true,
      },
      {
        id: "stopped",
        name: "Archive",
        urlLabel: "rtmp://example/live",
        status: {
          label: "Stopped",
          tone: "neutral",
          detail: "Stopped by operator",
        },
        encodingLabel: "source",
        rateLabel: "--",
        uptimeLabel: null,
        controlLabel: "Start",
        controlDisabled: false,
        monitorAvailable: false,
        deleteDisabled: false,
      },
    ],
    listCaption: null,
    expanded: false,
    canExpand: false,
  });
  const stopping = buildPipelineOutputOverviewModel(
    [
      pipeline({
        id: "live",
        outputs: [
          output({
            id: "retrying",
            status: "retrying",
            retrying: true,
          }),
        ],
      }),
    ],
    "live",
    [{ outputId: "retrying", intent: "stopping", busy: true }],
  );
  assert.equal(stopping.cards[0].controlLabel, "Stopping...");
  assert.equal(stopping.cards[0].controlDisabled, true);
  assert.equal(buildPipelineOutputOverviewModel([], "removed"), null);
});

test("pipeline output overview preserves the eight-card expansion boundary", async () => {
  const { buildPipelineOutputOverviewModel } = await loadCompiledFrontendModule(
    "features/pipeline-operate-view-model.js",
  );
  const outputs = Array.from({ length: 10 }, (_, index) =>
    output({ id: `out-${index}`, name: `Output ${index}` }),
  );
  const bounded = buildPipelineOutputOverviewModel(
    [pipeline({ id: "many", outputs })],
    "many",
  );
  assert.equal(bounded.cards.length, 8);
  assert.equal(bounded.listCaption, "Showing first 8 of 10 outputs");
  assert.equal(bounded.expanded, false);
  assert.equal(bounded.canExpand, true);

  const expanded = buildPipelineOutputOverviewModel(
    [pipeline({ id: "many", outputs })],
    "many",
    [],
    true,
  );
  assert.equal(expanded.cards.length, 10);
  assert.equal(expanded.listCaption, "Showing all 10 outputs");
  assert.equal(expanded.expanded, true);

  const redacted = buildPipelineOutputOverviewModel(
    [
      pipeline({
        id: "secret",
        outputs: [
          output({
            url: "rtmp://example.com/live/abcdefghijklmnopqrstuvwxyz",
          }),
        ],
      }),
    ],
    "secret",
  );
  assert.equal(redacted.cards[0].urlLabel, "rtmp://example.com/l***vwxyz");
});
