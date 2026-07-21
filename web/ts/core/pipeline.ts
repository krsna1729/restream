import type {
  AudioTrack,
  ConfigData,
  HealthData,
  IngestUrls,
  Job,
  PipelineView,
  VideoTrack,
} from "../types.js";
import { normalizeOutputConfig } from "./output-config.js";

type UnknownRecord = Record<string, unknown>;

const throughputState = {
  outputBytes: new Map<string, { ts: number; bytes: number }>(),
};

function isRecord(value: unknown): value is UnknownRecord {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function stringOrNull(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function nonEmptyString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function finiteNonNegativeNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0
    ? value
    : null;
}

function finiteNonNegativeInteger(value: unknown): number | null {
  const number = finiteNonNegativeNumber(value);
  return number !== null && Number.isInteger(number) ? number : null;
}

function nonNegativeNumberOrZero(value: unknown): number {
  return finiteNonNegativeNumber(value) ?? 0;
}

function timestampMs(value: unknown): number | null {
  if (typeof value !== "string" || value.length === 0) return null;
  const timestamp = new Date(value).getTime();
  return Number.isFinite(timestamp) ? timestamp : null;
}

function jobTimestamp(job: Job): number | null {
  return timestampMs(job.startedAt) ?? timestampMs(job.endedAt);
}

function computeKbps(
  stateMap: Map<string, { ts: number; bytes: number }>,
  key: string | null | undefined,
  totalBytes: number,
  nowMs: number,
): number | null {
  if (!key || finiteNonNegativeNumber(totalBytes) === null) return null;
  const prev = stateMap.get(key);
  stateMap.set(key, { ts: nowMs, bytes: totalBytes });

  if (!prev) return null;
  const dtMs = nowMs - prev.ts;
  if (dtMs <= 0) return null;

  const deltaBytes = Math.max(0, totalBytes - prev.bytes);
  return Number(((deltaBytes * 8) / (dtMs / 1000) / 1000).toFixed(1));
}

function resolveIngestUrls(pipeline: UnknownRecord): IngestUrls {
  const ingestUrls = isRecord(pipeline.ingestUrls) ? pipeline.ingestUrls : {};
  return {
    rtmp: stringOrNull(ingestUrls.rtmp),
    srt: stringOrNull(ingestUrls.srt),
  };
}

function mapVideoTrack(track: UnknownRecord): VideoTrack {
  const width = finiteNonNegativeInteger(track.width);
  const height = finiteNonNegativeInteger(track.height);
  const fps = finiteNonNegativeNumber(track.fps);
  const pid = finiteNonNegativeInteger(track.pid);
  return {
    codec: stringOrNull(track.codec) ?? undefined,
    width: width ?? undefined,
    height: height ?? undefined,
    fps: fps ?? undefined,
    pid: pid ?? null,
    language: stringOrNull(track.language),
    title: stringOrNull(track.title),
    profile: stringOrNull(track.profile) ?? undefined,
    level: stringOrNull(track.level) ?? undefined,
  };
}

function mapAudioTrack(track: UnknownRecord): AudioTrack {
  const index = finiteNonNegativeInteger(track.index ?? track.trackIndex);
  const pid = finiteNonNegativeInteger(track.pid);
  const channels = finiteNonNegativeInteger(track.channels);
  const sampleRate = finiteNonNegativeInteger(
    track.sampleRate ?? track.sample_rate,
  );
  return {
    index: index ?? undefined,
    pid: pid ?? null,
    codec: stringOrNull(track.codec) ?? undefined,
    channels: channels ?? undefined,
    sample_rate: sampleRate ?? undefined,
    language: stringOrNull(track.language),
    title: stringOrNull(track.title),
    profile: stringOrNull(track.profile) ?? undefined,
  };
}

function missingPipeline(pipelineId: string): PipelineView {
  return {
    id: pipelineId,
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
}

function parsePipelinesInfo(
  config: Partial<ConfigData>,
  health: Partial<HealthData>,
): PipelineView[] {
  const rawConfig: UnknownRecord = isRecord(config) ? config : {};
  const rawHealth: UnknownRecord = isRecord(health) ? health : {};
  const healthByPipeline: UnknownRecord = isRecord(rawHealth.pipelines)
    ? rawHealth.pipelines
    : {};
  const pipelineHealthFor = (pipelineId: string): UnknownRecord => {
    const value = healthByPipeline[pipelineId];
    return isRecord(value) ? value : {};
  };

  const newPipelines: PipelineView[] = [];
  const pipelineById = new Map<string, PipelineView>();
  const latestJobsByOutput = new Map<string, Job>();
  const activeOutputStateKeys = new Set<string>();
  const nowMs = Date.now();

  for (const rawJob of asArray(rawConfig.jobs)) {
    if (!isRecord(rawJob)) continue;
    const pipelineId = nonEmptyString(rawJob.pipelineId);
    const outputId = nonEmptyString(rawJob.outputId);
    if (!pipelineId || !outputId) continue;

    const job: Job = {
      pipelineId,
      outputId,
      startedAt: stringOrNull(rawJob.startedAt) ?? undefined,
      endedAt: stringOrNull(rawJob.endedAt) ?? undefined,
    };
    const key = `${pipelineId}:${outputId}`;
    const previous = latestJobsByOutput.get(key);
    if (!previous) {
      latestJobsByOutput.set(key, job);
      continue;
    }

    const previousTime = jobTimestamp(previous);
    const currentTime = jobTimestamp(job);
    if (
      currentTime !== null &&
      (previousTime === null || currentTime >= previousTime)
    ) {
      latestJobsByOutput.set(key, job);
    }
  }

  for (const rawPipeline of asArray(rawConfig.pipelines)) {
    if (!isRecord(rawPipeline)) continue;
    const pipelineId = nonEmptyString(rawPipeline.id);
    if (!pipelineId) continue;

    const pipelineHealth = pipelineHealthFor(pipelineId);
    const inputHealth = isRecord(pipelineHealth.input)
      ? pipelineHealth.input
      : {};
    const inputBytesReceived = nonNegativeNumberOrZero(
      inputHealth.bytesReceived,
    );
    const inputPublisher = isRecord(inputHealth.publisher)
      ? (inputHealth.publisher as unknown as PipelineView["input"]["publisher"])
      : null;
    const unexpectedReaders = isRecord(inputHealth.unexpectedReaders)
      ? inputHealth.unexpectedReaders
      : {};
    const unexpectedReadersCount = nonNegativeNumberOrZero(
      unexpectedReaders.count,
    );
    const inputVideo = isRecord(inputHealth.video)
      ? mapVideoTrack(inputHealth.video)
      : null;
    const rawInputAudio = isRecord(inputHealth.audio)
      ? inputHealth.audio
      : null;
    const inputAudioTracks = asArray(inputHealth.audioTracks)
      .filter(isRecord)
      .map(mapAudioTrack);
    if (inputAudioTracks.length === 0 && rawInputAudio) {
      inputAudioTracks.push(mapAudioTrack(rawInputAudio));
    }

    const rawInputKbps = finiteNonNegativeNumber(inputHealth.bitrateKbps);
    const inputKbps =
      rawInputKbps === null ? null : Number(rawInputKbps.toFixed(1));
    const inputLastProgressAgeMs = finiteNonNegativeNumber(
      inputHealth.lastProgressAgeMs,
    );

    if (inputVideo) inputVideo.bw = inputKbps;

    const rawInputStatus = stringOrNull(inputHealth.status) || "off";
    const disconnectGraceActive = inputHealth.disconnectGraceActive === true;
    const disconnectGraceRemainingMs = finiteNonNegativeNumber(
      inputHealth.disconnectGraceRemainingMs,
    );
    const inputStatus =
      rawInputStatus === "off" && disconnectGraceActive
        ? "warning"
        : rawInputStatus;
    const probeReady = inputHealth.probeReady === true;
    const probeStatus = stringOrNull(inputHealth.probeStatus) || "off";
    const probePendingMs = finiteNonNegativeNumber(inputHealth.probePendingMs);
    const lastDisconnectAgeMs = finiteNonNegativeNumber(
      inputHealth.lastDisconnectAgeMs,
    );
    const publishStartedAt = stringOrNull(inputHealth.publishStartedAt);
    const publishStartedTs = timestampMs(publishStartedAt);

    let inputTime: number | null = null;
    if (inputStatus === "on" && publishStartedTs !== null && publishStartedTs > 0) {
      inputTime = Math.max(0, nowMs - publishStartedTs);
    }

    const rawHlsPreview = isRecord(pipelineHealth.hlsPreview)
      ? pipelineHealth.hlsPreview
      : {};
    const hlsLastAccessAgeMs = finiteNonNegativeNumber(
      rawHlsPreview.lastAccessAgeMs,
    );
    const recording = isRecord(pipelineHealth.recording)
      ? pipelineHealth.recording
      : {};
    const pipeline: PipelineView = {
      id: pipelineId,
      name: stringOrNull(rawPipeline.name) || pipelineId,
      key: stringOrNull(rawPipeline.streamKey),
      inputSource: stringOrNull(rawPipeline.inputSource),
      srtIngestPolicy: isRecord(rawPipeline.srtIngestPolicy)
        ? (rawPipeline.srtIngestPolicy as unknown as PipelineView["srtIngestPolicy"])
        : null,
      ingestUrls: resolveIngestUrls(rawPipeline),
      fileIngest: isRecord(rawPipeline.fileIngest)
        ? (rawPipeline.fileIngest as unknown as PipelineView["fileIngest"])
        : null,
      input: {
        status: inputStatus,
        time: inputTime,
        probeReady,
        probeStatus,
        probePendingMs,
        video: inputVideo,
        videoTrackSelection: isRecord(inputHealth.videoTrackSelection)
          ? (inputHealth.videoTrackSelection as unknown as PipelineView["input"]["videoTrackSelection"])
          : null,
        audio: inputAudioTracks[0] || null,
        audioTracks: inputAudioTracks,
        bytesReceived: inputBytesReceived,
        bytesSent: nonNegativeNumberOrZero(inputHealth.bytesSent),
        readers: nonNegativeNumberOrZero(inputHealth.readers),
        bitrateKbps: inputKbps,
        lastProgressAgeMs: inputLastProgressAgeMs,
        publisher: inputPublisher,
        unexpectedReadersCount,
        lastSessionProtocol: stringOrNull(inputHealth.lastSessionProtocol),
        lastDisconnectAt: stringOrNull(inputHealth.lastDisconnectAt),
        lastDisconnectAgeMs,
        lastDisconnectReason: stringOrNull(inputHealth.lastDisconnectReason),
        lastFailurePhase: stringOrNull(inputHealth.lastFailurePhase),
        recentDisconnectError: inputHealth.recentDisconnectError === true,
        recentDisconnectCount: nonNegativeNumberOrZero(
          inputHealth.recentDisconnectCount,
        ),
        flapping: inputHealth.flapping === true,
        disconnectGraceActive,
        disconnectGraceRemainingMs,
        lastRemoteAddr: stringOrNull(inputHealth.lastRemoteAddr),
        lastSessionBytesReceived: finiteNonNegativeNumber(
          inputHealth.lastSessionBytesReceived,
        ),
      },
      outs: [],
      stats: {
        inputBitrateKbps: inputKbps,
        outputBitrateKbps: null,
        readerCount: nonNegativeNumberOrZero(inputHealth.readers),
        outputCount: 0,
        readerMismatch: false,
        unexpectedReadersCount,
      },
      recording: {
        enabled: recording.enabled === true,
        active: recording.active === true,
      },
      hlsPreview: {
        active: rawHlsPreview.active === true,
        persistentConsumers: nonNegativeNumberOrZero(
          rawHlsPreview.persistentConsumers,
        ),
        lastAccessAgeMs: hlsLastAccessAgeMs,
        segments: nonNegativeNumberOrZero(rawHlsPreview.segments),
        playlistBytes: nonNegativeNumberOrZero(rawHlsPreview.playlistBytes),
      },
    };
    newPipelines.push(pipeline);
    if (!pipelineById.has(pipelineId)) pipelineById.set(pipelineId, pipeline);
  }

  for (const rawOutput of asArray(rawConfig.outputs)) {
    if (!isRecord(rawOutput)) continue;
    const outputId = nonEmptyString(rawOutput.id);
    const pipelineId = nonEmptyString(rawOutput.pipelineId);
    if (!outputId || !pipelineId) continue;

    const outputStateKey = `${pipelineId}:${outputId}`;
    activeOutputStateKeys.add(outputStateKey);
    const outputConfig = normalizeOutputConfig(rawOutput);
    let pipe = pipelineById.get(pipelineId);
    const latestJob = latestJobsByOutput.get(outputStateKey);
    const pipelineHealth = pipelineHealthFor(pipelineId);
    const outputHealthById = isRecord(pipelineHealth.outputs)
      ? pipelineHealth.outputs
      : {};
    const outHealth = isRecord(outputHealthById[outputId])
      ? outputHealthById[outputId]
      : {};
    const status = stringOrNull(outHealth.status) || "off";
    const retrying = status === "retrying" || outHealth.retrying === true;
    const flapping = outHealth.flapping === true;

    if (!pipe) {
      console.error("Not found pipeline for output: ", rawOutput);
      pipe = missingPipeline(pipelineId);
      newPipelines.push(pipe);
      pipelineById.set(pipelineId, pipe);
    }

    const outputTotalSize = finiteNonNegativeNumber(outHealth.totalSize);
    // Always refresh a valid byte-counter baseline, even when ffmpeg supplies a
    // direct bitrate. A later fallback sample must compare with the immediately
    // preceding counter rather than a stale pre-direct sample.
    const computedOutputKbps =
      outputTotalSize === null
        ? null
        : computeKbps(
            throughputState.outputBytes,
            outputStateKey,
            outputTotalSize,
            nowMs,
          );
    const directOutputKbps = finiteNonNegativeNumber(outHealth.bitrateKbps);
    const outBitrateKbps = directOutputKbps ?? computedOutputKbps;

    let outTime: number | null = null;
    const runtimeUptimeSecs = finiteNonNegativeNumber(outHealth.uptimeSecs);
    if (
      (status === "on" || status === "running") &&
      runtimeUptimeSecs !== null
    ) {
      outTime = Math.round(runtimeUptimeSecs * 1000);
    } else if (
      (status === "on" || status === "running") &&
      latestJob?.startedAt
    ) {
      const startedAt = timestampMs(latestJob.startedAt);
      if (startedAt !== null) outTime = Math.max(0, nowMs - startedAt);
    }

    pipe.outs.push({
      id: outputId,
      pipe: pipe.name,
      name: stringOrNull(rawOutput.name) || outputId,
      desiredState: stringOrNull(rawOutput.desiredState) || "stopped",
      config: outputConfig,
      url: stringOrNull(rawOutput.url) || "",
      monitoringUrl: stringOrNull(rawOutput.monitoringUrl),
      status,
      rawStatus: stringOrNull(outHealth.rawStatus),
      phase: stringOrNull(outHealth.phase),
      failurePhase: stringOrNull(outHealth.failurePhase),
      lastError: stringOrNull(outHealth.lastError),
      lastErrorAt: stringOrNull(outHealth.lastErrorAt),
      lastProgressAt: stringOrNull(outHealth.lastProgressAt),
      lastProgressAgeMs: finiteNonNegativeNumber(outHealth.lastProgressAgeMs),
      recentFailureCount: nonNegativeNumberOrZero(outHealth.recentFailureCount),
      flapping,
      retrying,
      retryAttempts: finiteNonNegativeNumber(outHealth.retryAttempts),
      retryBackoffMs: finiteNonNegativeNumber(outHealth.retryBackoffMs),
      nextRetryAt: stringOrNull(outHealth.nextRetryAt),
      retryRemainingMs: finiteNonNegativeNumber(outHealth.retryRemainingMs),
      time: outTime,
      job: latestJob || null,
      totalSize: outputTotalSize,
      bitrateKbps: outBitrateKbps,
    });
  }

  for (const stateKey of throughputState.outputBytes.keys()) {
    if (!activeOutputStateKeys.has(stateKey)) {
      throughputState.outputBytes.delete(stateKey);
    }
  }

  newPipelines.forEach((pipe) => {
    const outputCount = pipe.outs.length;
    const readerCount = pipe.input.readers;

    const activeOutputKbps = pipe.outs
      .filter(
        (output) =>
          output.status === "on" ||
          output.status === "running" ||
          output.status === "warning",
      )
      .map((output) => output.bitrateKbps)
      .filter(
        (kbps): kbps is number =>
          kbps !== null && Number.isFinite(kbps) && kbps >= 0,
      );
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
      unexpectedReadersCount: pipe.input.unexpectedReadersCount,
    };
  });

  return newPipelines;
}

export { parsePipelinesInfo };
