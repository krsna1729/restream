export type OperatorStateName = "empty" | "mixed-health";

interface OperatorStateFixture {
  settings: Record<string, unknown>;
  runtime: Record<string, unknown>;
  logs: Record<string, unknown>[];
}

const generatedAt = "2026-07-14T06:30:00Z";

const baseMetrics = {
  generatedAt,
  cpu: { usagePercent: 18, cores: 8, load1: 0.7 },
  memory: {
    usedPercent: 42,
    totalBytes: 8_589_934_592,
    usedBytes: 3_607_772_160,
  },
  disk: { usedPercent: 36, mountPoint: "/", root: "/" },
  network: { downloadKbps: 120, uploadKbps: 4_800 },
  engine: {
    cpuPercent: 7,
    restreamCpuPercent: 5,
    externalFfmpegCpuPercent: 2,
    externalFfmpegCount: 1,
    totalMemoryBytes: 314_572_800,
    restreamMemoryBytes: 209_715_200,
    externalFfmpegMemoryBytes: 104_857_600,
    cpuSampleReady: true,
  },
};

export const operatorStates: Record<OperatorStateName, OperatorStateFixture> = {
  empty: {
    settings: {
      serverName: "Synthetic Restream",
      pipelines: [],
      outputs: [],
      jobs: [],
    },
    runtime: {
      health: { status: "ready", pipelines: {} },
      metrics: baseMetrics,
    },
    logs: [],
  },
  "mixed-health": {
    settings: {
      serverName: "Synthetic Restream",
      pipelines: [
        {
          id: "pipe-healthy",
          name: "Healthy Program",
          streamKey: "synthetic-healthy-key",
          ingestUrls: {
            rtmp: "rtmp://ingest.example.invalid/live/synthetic-healthy-key",
            srt: null,
          },
        },
        {
          id: "pipe-retrying",
          name: "Retrying Destination",
          streamKey: "synthetic-retrying-key",
          ingestUrls: {
            rtmp: "rtmp://ingest.example.invalid/live/synthetic-retrying-key",
            srt: null,
          },
        },
      ],
      outputs: [
        {
          id: "out-healthy",
          pipelineId: "pipe-healthy",
          name: "Healthy Output",
          desiredState: "started",
          url: "rtmp://destination.example.invalid/live/synthetic-healthy",
          encoding: "source",
        },
        {
          id: "out-retrying",
          pipelineId: "pipe-retrying",
          name: "Retrying Output",
          desiredState: "started",
          url: "rtmp://destination.example.invalid/live/synthetic-retrying",
          encoding: "source",
        },
      ],
      jobs: [],
    },
    runtime: {
      health: {
        status: "ready",
        pipelines: {
          "pipe-healthy": {
            input: {
              status: "on",
              bitrateKbps: 3_200,
              bytesReceived: 48_000_000,
              bytesSent: 45_000_000,
              readers: 1,
              video: { codec: "h264", width: 1920, height: 1080 },
              audioTracks: [
                {
                  trackIndex: 0,
                  pid: 257,
                  codec: "aac",
                  channels: 2,
                  sampleRate: 48_000,
                  language: "eng",
                },
              ],
            },
            outputs: {
              "out-healthy": {
                status: "running",
                uptimeSecs: 420,
                bytesSent: 22_000_000,
                bytesDelivered: 22_000_000,
                bitrateKbps: 2_900,
              },
            },
            recording: { enabled: false, active: false },
          },
          "pipe-retrying": {
            input: {
              status: "on",
              bitrateKbps: 2_400,
              bytesReceived: 31_000_000,
              bytesSent: 0,
              readers: 1,
              video: { codec: "h264", width: 1280, height: 720 },
              audioTracks: [],
            },
            outputs: {
              "out-retrying": {
                status: "retrying",
                retrying: true,
                retryAttempts: 3,
                retryBackoffMs: 15_000,
                retryRemainingMs: 6_000,
                recentFailureCount: 2,
                lastError: "Synthetic destination refused the connection",
                lastErrorAt: "2026-07-14T06:29:54Z",
                bytesSent: 0,
                bytesDelivered: 0,
              },
            },
            recording: { enabled: true, active: false },
          },
        },
      },
      metrics: baseMetrics,
    },
    logs: [
      {
        id: 101,
        ts: "2026-07-14T06:29:54Z",
        level: "WARN",
        target: "restream::media",
        message: "Synthetic output entered retry backoff",
        fields: "{}",
        pipelineId: "pipe-retrying",
        outputId: "out-retrying",
        eventType: "egress.retrying",
      },
    ],
  },
};
