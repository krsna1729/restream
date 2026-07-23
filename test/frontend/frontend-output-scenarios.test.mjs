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
    flapping: false,
    recentFailureCount: 0,
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

function requireModel(model) {
  assert.ok(model, "Expected a pipeline output overview model");
  return model;
}

runDomScenarioMatrix({
  suite: "output scenario matrix",
  async loadModules({ loadCompiledFrontendModule }) {
    const { buildPipelineOutputOverviewModel } = await loadCompiledFrontendModule(
      "features/pipeline-operate-view-model.js",
    );

    return {
      buildModel: (pipeline) =>
        requireModel(buildPipelineOutputOverviewModel([pipeline], pipeline.id)),
    };
  },
  scenarios: [
    {
      name: "healthy running output projects uptime, throughput, and monitor action",
      async run({ buildModel }) {
        const card = buildModel(makePipeline()).cards[0];

        assert.deepEqual(card.status, {
          label: "Running",
          tone: "success",
          detail: "Delivering media",
        });
        assert.equal(card.controlLabel, "Stop");
        assert.equal(card.uptimeLabel, "0:00:15");
        assert.equal(card.encodingLabel, "source");
        assert.equal(card.rateLabel, "1.5 Mb/s");
        assert.equal(card.monitorAvailable, true);
      },
    },
    {
      name: "retrying output keeps stop intent visible and surfaces retry countdown",
      async run({ buildModel }) {
        const card = buildModel(
          makePipeline({
            outs: [
              makeOutput({
                status: "retrying",
                retrying: true,
                retryAttempts: 3,
                retryBackoffMs: 15_000,
                retryRemainingMs: 6_000,
                phase: "connect",
                lastError: "connection reset by peer",
                totalSize: 0,
                bitrateKbps: null,
              }),
            ],
          }),
        ).cards[0];

        assert.deepEqual(card.status, {
          label: "Retrying",
          tone: "warning",
          detail: "Retry in 6s",
        });
        assert.equal(card.controlLabel, "Stop");
        assert.equal(card.deleteDisabled, true);
      },
    },
    {
      name: "flapping output shows recovered-but-unstable status",
      async run({ buildModel }) {
        const card = buildModel(
          makePipeline({
            outs: [makeOutput({ flapping: true, recentFailureCount: 4 })],
          }),
        ).cards[0];

        assert.deepEqual(card.status, {
          label: "Flapping",
          tone: "warning",
          detail: "4 recent failures",
        });
      },
    },
    {
      name: "stalled output surfaces progress age without pretending it is healthy",
      async run({ buildModel }) {
        const card = buildModel(
          makePipeline({
            outs: [
              makeOutput({
                status: "stalled",
                lastProgressAgeMs: 27_000,
                totalSize: 0,
                bitrateKbps: null,
              }),
            ],
          }),
        ).cards[0];

        assert.deepEqual(card.status, {
          label: "Stalled",
          tone: "warning",
          detail: "No progress for 27s",
        });
      },
    },
    {
      name: "stopped output flips to start and enables delete while hiding monitor-only affordances",
      async run({ buildModel }) {
        const card = buildModel(
          makePipeline({
            outs: [
              makeOutput({
                desiredState: "stopped",
                status: "off",
                time: null,
                monitoringUrl: null,
                totalSize: 0,
                bitrateKbps: null,
              }),
            ],
          }),
        ).cards[0];

        assert.deepEqual(card.status, {
          label: "Stopped",
          tone: "neutral",
          detail: "Stopped by operator",
        });
        assert.equal(card.controlLabel, "Start");
        assert.equal(card.uptimeLabel, null);
        assert.equal(card.rateLabel, "--");
        assert.equal(card.deleteDisabled, false);
        assert.equal(card.monitorAvailable, false);
      },
    },
  ],
});
