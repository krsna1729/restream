import type {
  AudioTrack,
  ConfigData,
  HealthData,
  HlsPreviewHealth,
  IngestUrls,
  Job,
  PipelineView,
  VideoTrack,
} from "../types.js";
import { normalizeOutputConfig } from "./output-config.js";

const throughputState = {
  outputBytes: new Map<string, { ts: number; bytes: number }>(),
};

function parseFiniteNumber(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (trimmed.length === 0) return null;
    const parsed = Number(trimmed);
    if (Number.isFinite(parsed)) return parsed;
  }
  return null;
}

function parseFiniteNumberOrZero(value: unknown): number {
  return parseFiniteNumber(value) ?? 0;
}

function parseKbps(value: unknown): number | null {
  const parsed = parseFiniteNumber(value);
  return parsed === null ? null : Number(parsed.toFixed(1));
}

function parseEpochMs(value: unknown): number | null {
  if (!value) return null;
  if (
    typeof value !== "string" &&
    typeof value !== "number" &&
    !(value instanceof Date)
  ) {
    return null;
  }

  const parsed = new Date(value).getTime();
  return Number.isFinite(parsed) ? parsed : null;
}

function computeKbps(
  stateMap: Map<string, { ts: number; bytes: number }>,
  key: string | null | undefined,
  totalBytes: number,
  nowMs: number,
): number | null {
  if (!key) return null;
  const safeBytes = Number(totalBytes || 0);
  const prev = stateMap.get(key);
  stateMap.set(key, { ts: nowMs, bytes: safeBytes });

  if (!prev) return null;
  const dtMs = nowMs - prev.ts;
  if (dtMs <= 0) return null;

  const deltaBytes = Math.max(0, safeBytes - prev.bytes);
  return Number(((deltaBytes * 8) / (dtMs / 1000) / 1000).toFixed(1));
}

function resolveIngestUrls(pipeline: { ingestUrls?: IngestUrls }): IngestUrls {
  return pipeline?.ingestUrls || { rtmp: null, srt: null };
}

function parsePipelinesInfo(
  config: Partial<ConfigData>,
  health: Partial<HealthData>,
): PipelineView[] {
  const newPipelines: PipelineView[] = [];
  const latestJobsByOutput = new Map<string, Job>();
  const healthByPipeline = health?.pipelines || {};
  const nowMs = Date.now();

  (config?.jobs || []).forEach((job) => {
    const key = `${job.pipelineId}:${job.outputId}`;
    const previous = latestJobsByOutput.get(key);
    const currentStart = parseEpochMs(job.startedAt || job.endedAt);
    const previousStart = previous
      ? parseEpochMs(previous.startedAt || previous.endedAt)
      : null;

    if (!previous) {
      latestJobsByOutput.set(key, job);
      return;
    }

    if (
      currentStart !== null &&
      (previousStart === null || currentStart >= previousStart)
    ) {
      latestJobsByOutput.set(key, job);
    }
  });

  (config?.pipelines || []).forEach((p) => {
    const inputHealth = healthByPipeline[p.id]?.input;
    const inputBytesReceived =
      parseFiniteNumberOrZero(inputHealth?.bytesReceived);
    const inputPublisher = healthByPipeline[p.id]?.input?.publisher || null;
    const unexpectedReadersCount = Number(
      parseFiniteNumberOrZero(inputHealth?.unexpectedReaders?.count),
    );
    const rawInputVideo = healthByPipeline[p.id]?.input?.video;
    const inputVideo: VideoTrack | null = rawInputVideo
      ? { ...rawInputVideo }
      : null;
    const rawInputAudio = healthByPipeline[p.id]?.input?.audio || null;
    const rawInputAudioTracks =
      healthByPipeline[p.id]?.input?.audioTracks || [];
    const mapAudioTrack = (track: any): AudioTrack => ({
      index: track.index !== undefined ? track.index : track.trackIndex,
      pid: track.pid ?? null,
      codec: track.codec,
      channels: track.channels,
      sample_rate:
        track.sampleRate !== undefined ? track.sampleRate : track.sample_rate,
      language: track.language ?? null,
      title: track.title ?? null,
      profile: track.profile,
    });
    const inputAudioTracks: AudioTrack[] =
      rawInputAudioTracks.length > 0
        ? rawInputAudioTracks.map(mapAudioTrack)
        : rawInputAudio
          ? [mapAudioTrack(rawInputAudio)]
          : [];
    const rawInputKbps = inputHealth?.bitrateKbps;
    const inputKbps = parseKbps(rawInputKbps);
    const rawInputProgressAgeMs = inputHealth?.lastProgressAgeMs;
    const inputLastProgressAgeMs = parseFiniteNumber(rawInputProgressAgeMs);

    if (inputVideo) inputVideo.bw = inputKbps;

    const rawInputStatus = healthByPipeline[p.id]?.input?.status || "off";
    const disconnectGraceActive = Boolean(
      healthByPipeline[p.id]?.input?.disconnectGraceActive,
    );
    const rawDisconnectGraceRemainingMs =
      healthByPipeline[p.id]?.input?.disconnectGraceRemainingMs;
    const disconnectGraceRemainingMs = parseFiniteNumber(
      rawDisconnectGraceRemainingMs,
    );
    const inputStatus =
      rawInputStatus === "off" && disconnectGraceActive
        ? "warning"
        : rawInputStatus;
    const probeReady = Boolean(healthByPipeline[p.id]?.input?.probeReady);
    const probeStatus = healthByPipeline[p.id]?.input?.probeStatus || "off";
    const rawProbePendingMs = healthByPipeline[p.id]?.input?.probePendingMs;
    const probePendingMs = parseFiniteNumber(rawProbePendingMs);
    const rawLastDisconnectAgeMs =
      healthByPipeline[p.id]?.input?.lastDisconnectAgeMs;
    const lastDisconnectAgeMs = parseFiniteNumber(rawLastDisconnectAgeMs);
    const publishStartedTs = parseEpochMs(
      healthByPipeline[p.id]?.input?.publishStartedAt,
    ) ?? NaN;

    let inputTime: number | null = null;
    if (
      inputStatus === "on" &&
      Number.isFinite(publishStartedTs) &&
      publishStartedTs > 0
    ) {
      inputTime = Math.max(0, nowMs - publishStartedTs);
    }

    const rawHlsPreview =
      (healthByPipeline[p.id] as { hlsPreview?: HlsPreviewHealth })?.hlsPreview ||
      (inputHealth as { hlsPreview?: HlsPreviewHealth })?.hlsPreview;
    const rawHlsLastAccessAgeMs = rawHlsPreview?.lastAccessAgeMs;
    const hlsLastAccessAgeMs = parseFiniteNumber(rawHlsLastAccessAgeMs);

    newPipelines.push({
      id: p.id,
      name: p.name,
      key: p.streamKey,
      inputSource: p.inputSource || null,
      srtIngestPolicy: p.srtIngestPolicy || null,
      ingestUrls: resolveIngestUrls(p),
      fileIngest: p.fileIngest || null,
      input: {
        status: inputStatus,
        time: inputTime,
        probeReady,
        probeStatus,
        probePendingMs,
        video: inputVideo,
        videoTrackSelection:
          healthByPipeline[p.id]?.input?.videoTrackSelection || null,
        audio: inputAudioTracks[0] || null,
        audioTracks: inputAudioTracks,
        bytesReceived: inputBytesReceived,
        bytesSent: parseFiniteNumberOrZero(inputHealth?.bytesSent),
        readers: parseFiniteNumberOrZero(inputHealth?.readers),
        bitrateKbps: inputKbps,
        lastProgressAgeMs: inputLastProgressAgeMs,
        publisher: inputPublisher ?? null,
        unexpectedReadersCount,
        lastSessionProtocol:
          healthByPipeline[p.id]?.input?.lastSessionProtocol || null,
        lastDisconnectAt:
          healthByPipeline[p.id]?.input?.lastDisconnectAt || null,
        lastDisconnectAgeMs,
        lastDisconnectReason:
          healthByPipeline[p.id]?.input?.lastDisconnectReason || null,
        lastFailurePhase:
          healthByPipeline[p.id]?.input?.lastFailurePhase || null,
        recentDisconnectError: Boolean(
          healthByPipeline[p.id]?.input?.recentDisconnectError,
        ),
        recentDisconnectCount:
          parseFiniteNumberOrZero(inputHealth?.recentDisconnectCount),
        flapping: Boolean(healthByPipeline[p.id]?.input?.flapping),
        disconnectGraceActive,
        disconnectGraceRemainingMs,
        lastRemoteAddr: healthByPipeline[p.id]?.input?.lastRemoteAddr || null,
        lastSessionBytesReceived:
          parseFiniteNumber(inputHealth?.lastSessionBytesReceived),
      },
      outs: [],
      stats: {
        inputBitrateKbps: inputKbps,
        outputBitrateKbps: null,
        readerCount: parseFiniteNumberOrZero(inputHealth?.readers),
        outputCount: 0,
        readerMismatch: false,
        unexpectedReadersCount,
      },
      recording: healthByPipeline[p.id]?.recording ?? {
        enabled: false,
        active: false,
      },
      hlsPreview: {
        active: Boolean(rawHlsPreview?.active),
        persistentConsumers: Math.max(
          0,
          parseFiniteNumberOrZero(rawHlsPreview?.persistentConsumers),
        ),
        lastAccessAgeMs: hlsLastAccessAgeMs,
        segments: Math.max(0, parseFiniteNumberOrZero(rawHlsPreview?.segments)),
        playlistBytes: Math.max(
          0,
          parseFiniteNumberOrZero(rawHlsPreview?.playlistBytes),
        ),
      },
    });
  });

  (config?.outputs || []).forEach((out) => {
    const config = normalizeOutputConfig(out);
    let pipe = newPipelines.find((p) => p.id === out.pipelineId);
    const latestJob = latestJobsByOutput.get(`${out.pipelineId}:${out.id}`);
    const outHealth =
      healthByPipeline[out.pipelineId]?.outputs?.[out.id] || null;
    const status = outHealth?.status || "off";
    const retrying =
      status === "retrying" || Boolean(outHealth?.retrying || false);
    const flapping = Boolean(outHealth?.flapping || false);

    if (!pipe) {
      console.error("Not found pipeline for output: ", out);
      pipe = {
        id: out.pipelineId,
        name: "Undefined",
        key: null,
        inputSource: null,
        srtIngestPolicy: null,
        fileIngest: null,
        input: {
          status: "off",
          time: null,
          probeReady: false,
          probeStatus: "off",
          probePendingMs: null,
          video: null,
          videoTrackSelection: null,
          audio: null,
          audioTracks: [],
          bitrateKbps: null,
          lastProgressAgeMs: null,
          bytesReceived: 0,
          bytesSent: 0,
          readers: 0,
          publisher: null,
          unexpectedReadersCount: 0,
          lastSessionProtocol: null,
          lastDisconnectAt: null,
          lastDisconnectAgeMs: null,
          lastDisconnectReason: null,
          lastFailurePhase: null,
          recentDisconnectError: false,
          recentDisconnectCount: 0,
          flapping: false,
          disconnectGraceActive: false,
          disconnectGraceRemainingMs: null,
          lastRemoteAddr: null,
          lastSessionBytesReceived: null,
        },
        ingestUrls: { rtmp: null, srt: null },
        outs: [],
        stats: {
          inputBitrateKbps: null,
          outputBitrateKbps: null,
          readerCount: 0,
          outputCount: 0,
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
      };
      newPipelines.push(pipe);
    }

    const outputTotalSize = parseFiniteNumber(outHealth?.totalSize);
    // Prefer the direct bitrate reading from ffmpeg progress (reliable for all protocols
    // including HLS where total_size may report N/A). Fall back to computing from byte delta.
    const outBitrateKbps =
      parseKbps(outHealth?.bitrateKbps) ??
      computeKbps(
        throughputState.outputBytes,
        `${out.pipelineId}:${out.id}`,
        outputTotalSize ?? 0,
        nowMs,
      );

    let outTime: number | null = null;
    const runtimeUptimeSecs = Number(outHealth?.uptimeSecs);
    if (
      (status === "on" || status === "running") &&
      Number.isFinite(runtimeUptimeSecs) &&
      runtimeUptimeSecs >= 0
    ) {
      outTime = Math.round(runtimeUptimeSecs * 1000);
    } else if (
      (status === "on" || status === "running") &&
      latestJob?.startedAt
    ) {
      const latestStartedAt = parseEpochMs(latestJob.startedAt);
      if (latestStartedAt !== null) {
        outTime = Math.max(0, nowMs - latestStartedAt);
      }
    }

    pipe.outs.push({
      id: out.id,
      pipe: pipe.name,
      name: out.name,
      desiredState: out.desiredState || "stopped",
      config,
      url: out.url,
      monitoringUrl: out.monitoringUrl || null,
      status,
      rawStatus: outHealth?.rawStatus || null,
      phase: outHealth?.phase || null,
      failurePhase: outHealth?.failurePhase || null,
      lastError: outHealth?.lastError || null,
      lastErrorAt: outHealth?.lastErrorAt || null,
      lastProgressAt: outHealth?.lastProgressAt || null,
      lastProgressAgeMs:
        parseFiniteNumber(outHealth?.lastProgressAgeMs),
      recentFailureCount:
        parseFiniteNumberOrZero(outHealth?.recentFailureCount),
      flapping,
      retrying,
      retryAttempts:
        parseFiniteNumber(outHealth?.retryAttempts),
      retryBackoffMs:
        parseFiniteNumber(outHealth?.retryBackoffMs),
      nextRetryAt: outHealth?.nextRetryAt || null,
      retryRemainingMs: parseFiniteNumber(outHealth?.retryRemainingMs),
      time: outTime,
      job: latestJob || null,
      totalSize: outputTotalSize,
      bitrateKbps: outBitrateKbps,
    });
  });

  newPipelines.forEach((pipe) => {
    const outputCount = pipe.outs.length;
    const readerCount = pipe.input.readers || 0;

    const activeOutputKbps = pipe.outs
      .filter(
        (o) =>
          o.status === "on" || o.status === "running" || o.status === "warning",
      )
      .map((o) => o.bitrateKbps)
      .filter((k): k is number => k !== null && k >= 0);
    const outputBitrateKbps =
      activeOutputKbps.length > 0
        ? Number(activeOutputKbps.reduce((a, b) => a + b, 0).toFixed(1))
        : null;

    pipe.stats = {
      inputBitrateKbps: pipe.input.bitrateKbps,
      outputBitrateKbps,
      readerCount,
      outputCount,
      readerMismatch: readerCount !== outputCount,
      unexpectedReadersCount: Number(pipe.input.unexpectedReadersCount || 0),
    };
  });

  return newPipelines;
}

export { parsePipelinesInfo };
