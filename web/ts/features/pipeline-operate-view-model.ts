import { outputViewEncodingLabel } from "../core/output-config.js";
import {
  maskSecret,
  msToHHMMSS,
  sanitizeLogMessage,
} from "../core/display.js";
import {
  isOutputFlapping,
  isOutputIntentStopped,
  isOutputRetrying,
  isOutputRunning,
  isOutputUnexpectedlyDown,
} from "../core/output-status.js";
import type { OutputView, PipelineView } from "../types.js";
import {
  formatOverviewBitrate,
  pipelineOverviewHealth,
} from "./overview-view-model.js";
import type { OverviewTone } from "./overview-view-model.js";
import type { OverviewStatus } from "./overview-view-model.js";
import {
  getPublisherQualityAlerts,
  normalizePublisherProtocolLabel,
} from "./publisher-quality.js";

export interface PipelineOperateSelectorItem {
  readonly id: string;
  readonly name: string;
  readonly selected: boolean;
  readonly statusLabel: string;
  readonly statusTone: OverviewTone;
  readonly inputRate: string;
  readonly outputRate: string;
  readonly runningOutputs: number;
  readonly totalOutputs: number;
}

export interface PipelineOperateSelectorModel {
  readonly selectedPipelineId: string | null;
  readonly pipelines: readonly PipelineOperateSelectorItem[];
}

export interface PipelineOperateHeaderModel {
  readonly id: string;
  readonly name: string;
  readonly health: OverviewStatus;
  readonly sourceLabel: string;
  readonly inputRate: string;
  readonly outputRate: string;
  readonly outputsLabel: string;
  readonly recordingLabel: string;
  readonly canDiagnose: boolean;
  readonly diagnoseDisabledReason?: string;
  readonly canEdit: boolean;
  readonly editDisabledReason?: string;
  readonly canDelete: boolean;
  readonly deleteTitle: string;
  readonly recordingControl: PipelineOperateLifecycleControlModel;
  readonly fileIngestControl: PipelineOperateLifecycleControlModel | null;
  readonly lifecycleMessages: readonly PipelineOperateLifecycleMessage[];
}

export interface PipelineOperateLifecycleControlModel {
  readonly label: string;
  readonly disabled: boolean;
  readonly title: string;
  readonly danger: boolean;
  readonly outlined: boolean;
}

export interface PipelineOperateLifecycleMessage {
  readonly id: "recording" | "file-ingest";
  readonly label: string;
  readonly detail: string;
  readonly tone: OverviewTone;
}

export interface PipelineOperateLifecycleControlSnapshot {
  readonly recordingIntent: "starting" | "stopping" | null;
  readonly fileIngestIntent: "starting" | "stopping" | null;
  readonly recordingError?: string | null;
  readonly fileIngestError?: string | null;
}

export interface PipelineOperateInputStatusModel {
  readonly id: string;
  readonly name: string;
  readonly status: OverviewStatus;
  readonly uptimeLabel: string;
  readonly publisherLabel: string;
  readonly publisherDetail: string;
  readonly publisherHealth: OverviewStatus | null;
  readonly preview: OverviewStatus;
  readonly previewDetail: string;
  readonly previewEnabled: boolean;
  readonly previewKeyAssigned: boolean;
  readonly videoLabel: string;
  readonly audioLabel: string;
  readonly unexpectedReadersLabel: string | null;
  readonly metricGroups: readonly PipelineOperateInputMetricGroup[];
  readonly liveSource: PipelineOperateLiveSourceModel | null;
  readonly fileSource: PipelineOperateFileSourceModel | null;
  readonly audioTracks: readonly PipelineOperateAudioTrackModel[];
}

export interface PipelineOperateAudioTrackModel {
  readonly key: string;
  readonly index: number;
  readonly label: string;
  readonly identity: string;
  readonly codec: string;
  readonly sampleRate: string;
  readonly channels: string;
  readonly profile: string;
  readonly editing: boolean;
  readonly draft: string;
}

export interface PipelineOperateFileSourceModel {
  readonly filename: string;
  readonly details: readonly PipelineOperateInputMetric[];
  readonly warning: string | null;
}

export interface PipelineOperateLiveSourceModel {
  readonly pipelineId: string;
  readonly streamKeyLabel: string;
  readonly protocols: readonly PipelineOperateIngestProtocolModel[];
}

export interface PipelineOperateIngestProtocolModel {
  readonly id: "rtmp" | "srt";
  readonly label: string;
  readonly selected: boolean;
  readonly urlLabel: string;
}

export interface PipelineOperateInputMetricGroup {
  readonly key: "traffic" | "video";
  readonly label: string;
  readonly metrics: readonly PipelineOperateInputMetric[];
}

export interface PipelineOperateInputMetric {
  readonly key: string;
  readonly label: string;
  readonly value: string;
}

export interface PipelineOutputOverviewCount {
  readonly key: string;
  readonly label: string;
  readonly count: number;
  readonly tone: OverviewTone;
}

export interface PipelineOutputAttentionItem {
  readonly id: string;
  readonly name: string;
  readonly status: OverviewStatus;
  readonly encodingLabel: string;
  readonly rateLabel: string;
}

export interface PipelineOutputControlSnapshot {
  readonly outputId: string;
  readonly intent: "starting" | "stopping" | null;
  readonly busy: boolean;
  readonly error?: string | null;
}

export interface PipelineOutputCardModel {
  readonly id: string;
  readonly name: string;
  readonly urlLabel: string;
  readonly status: OverviewStatus;
  readonly encodingLabel: string;
  readonly rateLabel: string;
  readonly uptimeLabel: string | null;
  readonly controlLabel: string;
  readonly controlDisabled: boolean;
  readonly controlError: string | null;
  readonly monitorAvailable: boolean;
  readonly deleteDisabled: boolean;
}

export interface PipelineOutputOverviewModel {
  readonly pipelineId: string;
  readonly pipelineName: string;
  readonly activeLabel: string;
  readonly aggregateRate: string;
  readonly counts: readonly PipelineOutputOverviewCount[];
  readonly attention: readonly PipelineOutputAttentionItem[];
  readonly cards: readonly PipelineOutputCardModel[];
  readonly listCaption: string | null;
  readonly expanded: boolean;
  readonly canExpand: boolean;
}

export const PIPELINE_OUTPUT_CARD_LIMIT = 8;

interface OutputPresentationStatus {
  readonly key: string;
  readonly status: OverviewStatus;
  readonly score: number;
}

function formatUptime(ms: number | null): string {
  if (ms === null) return "No active session";
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return `${hours}:${minutes.toString().padStart(2, "0")}:${seconds
    .toString()
    .padStart(2, "0")} uptime`;
}

function inputStatus(pipeline: PipelineView): OverviewStatus {
  if (pipeline.input.status === "on") {
    if (!pipeline.input.probeReady) {
      const pending = pipeline.input.probePendingMs;
      return {
        label: "Probing",
        tone: "warning",
        detail: pending
          ? `Waiting ${(pending / 1000).toFixed(1)}s`
          : "Waiting for metadata",
      };
    }
    if (pipeline.input.flapping) {
      return {
        label: "Live, unstable",
        tone: "warning",
        detail: "Recent disconnects",
      };
    }
    return { label: "Live input", tone: "success", detail: "Receiving media" };
  }
  if (pipeline.input.disconnectGraceActive) {
    return {
      label: "Reconnecting",
      tone: "warning",
      detail: "Disconnect grace active",
    };
  }
  if (pipeline.input.recentDisconnectError) {
    return {
      label: "Input offline",
      tone: "error",
      detail: pipeline.input.lastDisconnectReason || "Recent ingest failure",
    };
  }
  return { label: "Input offline", tone: "neutral", detail: "Awaiting source" };
}

function retryDetail(output: OutputView): string {
  if (
    output.retryRemainingMs !== null &&
    Number.isFinite(output.retryRemainingMs) &&
    output.retryRemainingMs >= 0
  ) {
    return `Retry in ${Math.round(output.retryRemainingMs / 1000)}s`;
  }
  if (output.retryAttempts !== null && output.retryAttempts > 0) {
    return `Retry attempt ${output.retryAttempts}`;
  }
  return output.lastError || "Retry queued";
}

function outputPresentationStatus(
  output: OutputView,
): OutputPresentationStatus {
  if (isOutputIntentStopped(output)) {
    return {
      key: "stopped",
      status: {
        label: "Stopped",
        tone: "neutral",
        detail: "Stopped by operator",
      },
      score: 0,
    };
  }
  if (isOutputRetrying(output)) {
    return {
      key: "retrying",
      status: {
        label: "Retrying",
        tone: "warning",
        detail: retryDetail(output),
      },
      score: 80,
    };
  }
  if (
    output.lastError ||
    output.status === "failed" ||
    output.status === "error"
  ) {
    return {
      key: "error",
      status: {
        label: "Error",
        tone: "error",
        detail: output.lastError || output.failurePhase || "Output failed",
      },
      score: 90,
    };
  }
  if (isOutputFlapping(output)) {
    return {
      key: "flapping",
      status: {
        label: "Flapping",
        tone: "warning",
        detail: `${Math.max(output.recentFailureCount, 2)} recent failures`,
      },
      score: 70,
    };
  }
  if (output.status === "stalled" || output.status === "warning") {
    return {
      key: "warning",
      status: {
        label: output.status === "stalled" ? "Stalled" : "Warning",
        tone: "warning",
        detail:
          output.status === "stalled" && output.lastProgressAgeMs !== null
            ? `No progress for ${Math.round(output.lastProgressAgeMs / 1000)}s`
            : output.phase || "Output needs attention",
      },
      score: 60,
    };
  }
  if (isOutputUnexpectedlyDown(output)) {
    return {
      key: "down",
      status: {
        label: "Down",
        tone: "error",
        detail: output.lastError || "Expected output is not running",
      },
      score: 100,
    };
  }
  if (isOutputRunning(output)) {
    return {
      key: "running",
      status: { label: "Running", tone: "success", detail: "Delivering media" },
      score: 0,
    };
  }
  return {
    key: "other",
    status: {
      label: "Other",
      tone: "neutral",
      detail: output.phase || output.status,
    },
    score: 0,
  };
}

export function buildPipelineOperateSelectorModel(
  pipelines: readonly PipelineView[],
  selectedPipelineId: string | null,
): PipelineOperateSelectorModel {
  const selectionIsValid = pipelines.some(
    (pipeline) => pipeline.id === selectedPipelineId,
  );
  const selectedId = selectionIsValid ? selectedPipelineId : null;
  return {
    selectedPipelineId: selectedId,
    pipelines: [...pipelines]
      .sort((left, right) => left.name.localeCompare(right.name))
      .map((pipeline) => {
        const health = pipelineOverviewHealth(pipeline);
        const runningOutputs = pipeline.outs.filter(isOutputRunning).length;
        return {
          id: pipeline.id,
          name: pipeline.name,
          selected: pipeline.id === selectedId,
          statusLabel: health.label,
          statusTone: health.tone,
          inputRate: formatOverviewBitrate(pipeline.stats.inputBitrateKbps),
          outputRate: formatOverviewBitrate(pipeline.stats.outputBitrateKbps),
          runningOutputs,
          totalOutputs: pipeline.outs.length,
        };
      }),
  };
}

export function buildPipelineOperateHeaderModel(
  pipelines: readonly PipelineView[],
  selectedPipelineId: string | null,
  controls: PipelineOperateLifecycleControlSnapshot = {
    recordingIntent: null,
    fileIngestIntent: null,
  },
): PipelineOperateHeaderModel | null {
  const pipeline = pipelines.find(({ id }) => id === selectedPipelineId);
  if (!pipeline) return null;
  const runningOutputs = pipeline.outs.filter(isOutputRunning).length;
  const inputOnline = pipeline.input.status === "on";
  const recordingActive = pipeline.recording.active;
  const inputSource = (pipeline.inputSource || "").trim();
  const filename =
    pipeline.fileIngest?.filename ||
    (inputSource.startsWith("file:") ? inputSource.slice("file:".length) : "");
  let sourceLabel = "Awaiting publisher";
  if (filename) {
    sourceLabel = `File · ${filename}`;
  } else if (pipeline.input.publisher?.protocol) {
    sourceLabel = pipeline.input.publisher.protocol.toUpperCase();
  } else if (pipeline.ingestUrls?.rtmp) {
    sourceLabel = "RTMP";
  } else if (pipeline.ingestUrls?.srt) {
    sourceLabel = "SRT";
  }
  const recordingEnabled = pipeline.recording.enabled;
  const recordingPending = controls.recordingIntent !== null;
  const canStartRecording = inputOnline || recordingEnabled;
  const fileIngest = pipeline.fileIngest || null;
  const fileIngestConfigured = Boolean(
    inputSource.startsWith("file:") && fileIngest?.configured && fileIngest.id,
  );
  const fileIngestRunning = Boolean(fileIngest?.running);
  const fileIngestPending = controls.fileIngestIntent !== null;
  const canDelete = !pipeline.outs.some((output) => output.status !== "off");
  const lifecycleMessages: PipelineOperateLifecycleMessage[] = [];
  if (controls.recordingError) {
    lifecycleMessages.push({
      id: "recording",
      label: "Recording request failed",
      detail: controls.recordingError,
      tone: "error",
    });
  }
  if (controls.fileIngestError) {
    lifecycleMessages.push({
      id: "file-ingest",
      label: "File ingest request failed",
      detail: controls.fileIngestError,
      tone: "error",
    });
  }
  return {
    id: pipeline.id,
    name: pipeline.name,
    health: pipelineOverviewHealth(pipeline),
    sourceLabel,
    inputRate: formatOverviewBitrate(pipeline.stats.inputBitrateKbps),
    outputRate: formatOverviewBitrate(pipeline.stats.outputBitrateKbps),
    outputsLabel: `${runningOutputs}/${pipeline.outs.length} outputs`,
    recordingLabel: recordingActive
      ? "Recording active"
      : pipeline.recording.enabled
        ? "Recording armed"
        : "Recording off",
    canDiagnose: inputOnline,
    diagnoseDisabledReason: inputOnline
      ? undefined
      : "Input must be online to run diagnostics",
    canEdit: !recordingActive,
    editDisabledReason: recordingActive
      ? "Stop recording before editing"
      : undefined,
    canDelete,
    deleteTitle: canDelete ? "" : "Stop all outputs before deleting the pipeline",
    recordingControl: {
      label: recordingPending
        ? controls.recordingIntent === "starting"
          ? "Starting..."
          : "Stopping..."
        : recordingEnabled
          ? "Stop Rec"
          : "Record",
      disabled: recordingPending || !canStartRecording,
      title:
        !recordingPending && !canStartRecording
          ? "Input must be on to start recording"
          : "",
      danger:
        controls.recordingIntent === "stopping" ||
        (!recordingPending && recordingEnabled),
      outlined: controls.recordingIntent !== "starting" && !recordingEnabled,
    },
    fileIngestControl: fileIngestConfigured
      ? {
          label: fileIngestPending
            ? controls.fileIngestIntent === "starting"
              ? "Starting File..."
              : "Stopping File..."
            : fileIngestRunning
              ? "Stop File"
              : "Start File",
          disabled: fileIngestPending,
          title: fileIngest?.filename
            ? `${fileIngestRunning ? "Stop" : "Start"} file ingest for ${fileIngest.filename}`
            : "",
          danger:
            controls.fileIngestIntent === "stopping" ||
            (!fileIngestPending && fileIngestRunning),
          outlined:
            controls.fileIngestIntent !== "starting" && !fileIngestRunning,
        }
      : null,
    lifecycleMessages,
  };
}

export function buildPipelineOperateInputStatusModel(
  pipelines: readonly PipelineView[],
  selectedPipelineId: string | null,
  selectedProtocol: "rtmp" | "srt" = "rtmp",
  fileSourceModel: PipelineOperateFileSourceModel | null = null,
  audioTracks: readonly PipelineOperateAudioTrackModel[] = [],
): PipelineOperateInputStatusModel | null {
  const pipeline = pipelines.find(({ id }) => id === selectedPipelineId);
  if (!pipeline) return null;

  const publisher = pipeline.input.publisher;
  const publisherAlerts = getPublisherQualityAlerts(publisher);
  const fileSource = (pipeline.inputSource || "").startsWith("file:");
  const publisherLabel = publisher
    ? normalizePublisherProtocolLabel(publisher.protocol)
    : fileSource
      ? "File ingest"
      : "No publisher";
  const publisherDetail =
    publisher?.remoteAddr || (fileSource ? "Local media" : "Not connected");
  const publisherHealth = publisher
    ? publisherAlerts.length
      ? {
          label: "Needs attention",
          tone: "warning" as const,
          detail: publisherAlerts.map(({ label }) => label).join("; "),
        }
      : { label: "Healthy", tone: "success" as const, detail: "Publisher link" }
    : null;
  const previewActive = pipeline.hlsPreview.active;
  const previewUsed =
    pipeline.hlsPreview.segments > 0 ||
    pipeline.hlsPreview.persistentConsumers > 0;
  const preview: OverviewStatus = previewActive
    ? { label: "Preview live", tone: "success", detail: "HLS segmenter active" }
    : pipeline.input.status === "on"
      ? {
          label: previewUsed ? "Preview idle" : "Preview on demand",
          tone: "neutral",
          detail: previewUsed
            ? "No active browser consumer"
            : "Starts when opened",
        }
      : {
          label: "Preview unavailable",
          tone: "neutral",
          detail: "Input is offline",
        };
  const video = pipeline.input.video;
  const videoParts = video
    ? [
        video.codec ? video.codec.toUpperCase() : null,
        video.width && video.height ? `${video.width}×${video.height}` : null,
        video.fps !== null && video.fps !== undefined
          ? `${video.fps} fps`
          : null,
      ].filter(Boolean)
    : [];
  const audioCount = pipeline.input.audioTracks.length;
  const unexpectedCount = pipeline.input.unexpectedReadersCount;
  const previewConsumers = pipeline.hlsPreview.persistentConsumers;
  const availableProtocols = (["rtmp", "srt"] as const).filter(
    (protocol) => Boolean(pipeline.ingestUrls[protocol]?.trim()),
  );
  const activeProtocol = availableProtocols.includes(selectedProtocol)
    ? selectedProtocol
    : availableProtocols[0] || "rtmp";
  const videoSelection = pipeline.input.videoTrackSelection;
  const selectedTrackIndex = videoSelection?.selectedTrackIndex;
  const showVideoSelection =
    videoSelection &&
    videoSelection.availableTrackCount > 1 &&
    typeof selectedTrackIndex === "number";
  const videoMetrics: PipelineOperateInputMetric[] = [
    {
      key: "codec",
      label: "Codec",
      value: video?.codec?.toUpperCase() || "--",
    },
    {
      key: "resolution",
      label: "Resolution",
      value:
        video?.width && video.height ? `${video.width}×${video.height}` : "--",
    },
    {
      key: "fps",
      label: "FPS",
      value:
        video?.fps !== null && video?.fps !== undefined
          ? String(video.fps)
          : "--",
    },
    { key: "profile", label: "Profile", value: video?.profile || "--" },
    { key: "level", label: "Level", value: video?.level || "--" },
  ];
  if (Number.isFinite(video?.pid as number)) {
    videoMetrics.push({
      key: "pid",
      label: "PID",
      value: `0x${Number(video?.pid).toString(16).toUpperCase()}`,
    });
  }
  if (showVideoSelection) {
    videoMetrics.push({
      key: "selection",
      label: "Track selection",
      value: `Track ${selectedTrackIndex + 1} of ${videoSelection.availableTrackCount}`,
    });
  }

  return {
    id: pipeline.id,
    name: pipeline.name,
    status: inputStatus(pipeline),
    uptimeLabel: formatUptime(pipeline.input.time),
    publisherLabel,
    publisherDetail,
    publisherHealth,
    preview,
    previewDetail: `${pipeline.hlsPreview.segments} segments · ${previewConsumers} viewer${previewConsumers === 1 ? "" : "s"}`,
    previewEnabled: pipeline.input.status !== "off",
    previewKeyAssigned: Boolean(pipeline.key),
    videoLabel: videoParts.join(" · ") || "Waiting for media metadata",
    audioLabel: `${audioCount} audio track${audioCount === 1 ? "" : "s"}`,
    unexpectedReadersLabel:
      unexpectedCount > 0
        ? `${unexpectedCount} unexpected reader${unexpectedCount === 1 ? "" : "s"}`
        : null,
    metricGroups:
      pipeline.input.status !== "off"
        ? [
            {
              key: "traffic",
              label: "Traffic",
              metrics: [
                {
                  key: "input-rate",
                  label: "Input bitrate",
                  value: formatOverviewBitrate(
                    pipeline.stats.inputBitrateKbps,
                  ),
                },
                {
                  key: "output-rate",
                  label: "Output bitrate",
                  value: formatOverviewBitrate(
                    pipeline.stats.outputBitrateKbps,
                  ),
                },
                {
                  key: "readers",
                  label: "Readers",
                  value: String(pipeline.stats.readerCount),
                },
                {
                  key: "outputs",
                  label: "Outputs",
                  value: String(pipeline.stats.outputCount),
                },
              ],
            },
            { key: "video", label: "Video", metrics: videoMetrics },
          ]
        : [],
    liveSource: fileSource
      ? null
      : {
          pipelineId: pipeline.id,
          streamKeyLabel: maskSecret(pipeline.key),
          protocols: availableProtocols.map((protocol) => {
            const url = pipeline.ingestUrls[protocol] || "";
            return {
              id: protocol,
              label: protocol.toUpperCase(),
              selected: protocol === activeProtocol,
              urlLabel: pipeline.key
                ? url.replace(pipeline.key, maskSecret(pipeline.key))
                : url,
            };
          }),
        },
    fileSource: fileSource ? fileSourceModel : null,
    audioTracks,
  };
}

export function buildPipelineOutputOverviewModel(
  pipelines: readonly PipelineView[],
  selectedPipelineId: string | null,
  controlSnapshots: readonly PipelineOutputControlSnapshot[] = [],
  expanded = false,
): PipelineOutputOverviewModel | null {
  const pipeline = pipelines.find(({ id }) => id === selectedPipelineId);
  if (!pipeline) return null;

  const statusRows = pipeline.outs.map((output) => ({
    output,
    presentation: outputPresentationStatus(output),
  }));
  const countOrder = [
    ["down", "Down", "error"],
    ["error", "Error", "error"],
    ["retrying", "Retrying", "warning"],
    ["flapping", "Flapping", "warning"],
    ["warning", "Warning", "warning"],
    ["running", "Running", "success"],
    ["stopped", "Stopped", "neutral"],
    ["other", "Other", "neutral"],
  ] as const;
  const counts = countOrder
    .map(([key, label, tone]) => ({
      key,
      label,
      tone,
      count: statusRows.filter(({ presentation }) => presentation.key === key)
        .length,
    }))
    .filter(({ count }) => count > 0);
  const active = pipeline.outs.filter(isOutputRunning).length;
  const aggregateKbps = pipeline.outs.reduce(
    (sum, output) => sum + Math.max(0, output.bitrateKbps || 0),
    0,
  );
  const controlsByOutputId = new Map(
    controlSnapshots.map((snapshot) => [snapshot.outputId, snapshot]),
  );
  const attention = statusRows
    .filter(({ presentation }) => presentation.score > 0)
    .sort(
      (left, right) =>
        right.presentation.score - left.presentation.score ||
        left.output.name.localeCompare(right.output.name),
    )
    .slice(0, 5)
    .map(({ output, presentation }) => {
      return {
        id: output.id,
        name: output.name,
        status: presentation.status,
        encodingLabel: outputViewEncodingLabel(output),
        rateLabel: formatOverviewBitrate(output.bitrateKbps),
      };
    });
  const renderedRows = expanded
    ? statusRows
    : statusRows.slice(0, PIPELINE_OUTPUT_CARD_LIMIT);
  const cards = renderedRows.map(({ output, presentation }) => {
    const control = controlsByOutputId.get(output.id);
    const stopped = isOutputIntentStopped(output);
    return {
      id: output.id,
      name: output.name,
      urlLabel: sanitizeLogMessage(output.url || "", true),
      status: presentation.status,
      encodingLabel: outputViewEncodingLabel(output),
      rateLabel: formatOverviewBitrate(output.bitrateKbps),
      uptimeLabel:
        isOutputRunning(output) && Number.isFinite(output.time)
          ? msToHHMMSS(output.time)
          : null,
      controlLabel:
        control?.intent === "starting"
          ? "Starting..."
          : control?.intent === "stopping"
            ? "Stopping..."
            : stopped
              ? "Start"
              : "Stop",
      controlDisabled: Boolean(control?.busy || control?.intent),
      controlError: control?.error || null,
      monitorAvailable: Boolean(output.monitoringUrl),
      deleteDisabled: !stopped,
    };
  });
  const canExpand = statusRows.length > PIPELINE_OUTPUT_CARD_LIMIT;

  return {
    pipelineId: pipeline.id,
    pipelineName: pipeline.name,
    activeLabel: `${active}/${pipeline.outs.length} active`,
    aggregateRate: formatOverviewBitrate(aggregateKbps),
    counts,
    attention,
    cards,
    listCaption: canExpand
      ? expanded
        ? `Showing all ${statusRows.length} outputs`
        : `Showing first ${cards.length} of ${statusRows.length} outputs`
      : null,
    expanded,
    canExpand,
  };
}
