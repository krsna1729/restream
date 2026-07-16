import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

function pipeline({
  id,
  inputStatus = "on",
  probeReady = true,
  inputFlapping = false,
  outputs = [],
  inputKbps = 0,
  outputKbps = 0,
  recording = false,
  lastProgressAgeMs = null,
  bytesReceived = 0,
}) {
  return {
    id,
    name: id,
    input: {
      status: inputStatus,
      probeReady,
      flapping: inputFlapping,
      recentDisconnectCount: inputFlapping ? 3 : 0,
      publisher: null,
      probePendingMs: null,
      lastProgressAgeMs,
      bytesReceived,
    },
    outs: outputs,
    stats: {
      inputBitrateKbps: inputKbps,
      outputBitrateKbps: outputKbps,
    },
    recording: { active: recording, enabled: recording },
    inputSource: null,
  };
}

test("overview view model derives one immutable read-only fleet snapshot", async () => {
  const { buildOverviewViewModel } = await loadCompiledFrontendModule(
    "features/overview-view-model.js",
  );
  const pipelines = [
    pipeline({
      id: "healthy",
      outputs: [{ status: "running", desiredState: "started" }],
      inputKbps: 3200,
      outputKbps: 2900,
    }),
    pipeline({
      id: "retrying",
      outputs: [
        {
          status: "retrying",
          desiredState: "started",
          retrying: true,
        },
      ],
      inputKbps: 2400,
      recording: true,
    }),
    pipeline({
      id: "intentionally-stopped",
      inputStatus: "off",
      outputs: [{ status: "off", desiredState: "stopped" }],
    }),
  ];

  const model = buildOverviewViewModel(pipelines);
  assert.deepEqual(model.counts, {
    pipelines: 3,
    liveInputs: 2,
    warningInputs: 0,
    outputs: 3,
    runningOutputs: 1,
    retryingOutputs: 1,
    flappingOutputs: 0,
    stoppedOutputs: 1,
    downOutputs: 0,
    recording: 1,
    inputKbps: 5600,
    outputKbps: 2900,
  });
  assert.equal(model.attentionPipelines, 1);
  assert.deepEqual(model.attention, [
    {
      pipelineId: "retrying",
      pipelineName: "retrying",
      status: {
        label: "Output retrying",
        tone: "warning",
        detail: "recovering",
      },
      detail: "1 retrying",
    },
  ]);
  assert.equal(
    model.pipelines.find(({ id }) => id === "healthy").health.label,
    "Live",
  );
  assert.equal(
    model.pipelines.find(({ id }) => id === "retrying").outputs.label,
    "1 retrying",
  );
  assert.equal(
    model.pipelines.find(({ id }) => id === "intentionally-stopped").outputs
      .label,
    "Stopped",
  );
  assert.deepEqual(
    model.metrics.map(({ key, value }) => ({ key, value })),
    [
      { key: "inputs", value: "2/3" },
      { key: "outputs", value: "1/3" },
      { key: "inputKbps", value: "5.6 Mb/s" },
      { key: "outputKbps", value: "2.9 Mb/s" },
      { key: "engineCpu", value: "--" },
      { key: "engineMemory", value: "--" },
    ],
  );
  assert.deepEqual(model.activity, []);
});

test("overview attention includes probing and flapping inputs", async () => {
  const { buildOverviewViewModel } = await loadCompiledFrontendModule(
    "features/overview-view-model.js",
  );
  const model = buildOverviewViewModel([
    pipeline({ id: "probing", probeReady: false }),
    pipeline({ id: "flapping", inputFlapping: true }),
  ]);

  assert.equal(model.counts.warningInputs, 2);
  assert.equal(model.attentionPipelines, 2);
});

test("overview carries master's stalled-input refinement into both renderers", async () => {
  const { buildOverviewViewModel } = await loadCompiledFrontendModule(
    "features/overview-view-model.js",
  );
  const model = buildOverviewViewModel([
    pipeline({
      id: "stalled",
      lastProgressAgeMs: 12_000,
      bytesReceived: 48 * 1024,
    }),
  ]);

  assert.equal(model.counts.liveInputs, 0);
  assert.equal(model.counts.warningInputs, 1);
  assert.equal(model.attentionPipelines, 1);
  assert.deepEqual(model.pipelines[0].health, {
    label: "Input stalled",
    tone: "warning",
    detail: "no progress for 12s",
  });
  assert.deepEqual(model.pipelines[0].input, {
    label: "Input stalled",
    tone: "warning",
    detail: "publisher / 48.0 KiB received / stale 12s",
  });
  assert.equal(model.attention[0].detail, "input stale 12s");
});

test("overview keeps stalled outputs distinct from down outputs", async () => {
  const { buildOverviewViewModel } = await loadCompiledFrontendModule(
    "features/overview-view-model.js",
  );
  const model = buildOverviewViewModel([
    pipeline({
      id: "stalled-output",
      outputs: [{ status: "stalled", desiredState: "started" }],
    }),
  ]);

  assert.equal(model.attention[0].status.label, "Output stalled");
  assert.equal(model.attention[0].status.tone, "warning");
  assert.equal(model.attention[0].status.detail, "no progress");
});

test("overview view model carries engine history and semantic activity", async () => {
  const { buildOverviewViewModel } = await loadCompiledFrontendModule(
    "features/overview-view-model.js",
  );
  const model = buildOverviewViewModel(
    [],
    {
      engine: {
        cpuPercent: 12.4,
        restreamCpuPercent: 8,
        externalFfmpegCount: 1,
        externalFfmpegCpuPercent: 4.4,
        restreamMemoryBytes: 1024 * 1024,
        externalFfmpegMemoryBytes: 2 * 1024 * 1024,
      },
    },
    {
      metricHistory: { engineCpu: [10, 12.4] },
      activityBursts: [
        {
          badgeClass: "badge-error",
          detailBadges: ["Server Task Exit"],
          headline: "Restream task exited",
          logs: [{}, {}],
          summary: "A runtime task stopped unexpectedly.",
          startedAt: "2026-07-14T12:00:00Z",
          endedAt: "2026-07-14T12:00:01Z",
        },
      ],
    },
  );

  assert.equal(model.metrics[4].value, "12%");
  assert.deepEqual(model.metrics[4].history, [10, 12.4]);
  assert.equal(model.metrics[5].value, "3.0 MiB");
  assert.deepEqual(model.activity, [
    {
      headline: "Restream task exited",
      summary: "A runtime task stopped unexpectedly.",
      details: ["Server Task Exit"],
      eventCount: 2,
      startedAt: "2026-07-14T12:00:00Z",
      endedAt: "2026-07-14T12:00:01Z",
      tone: "error",
    },
  ]);
});
