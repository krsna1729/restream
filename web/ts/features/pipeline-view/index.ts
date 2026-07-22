import {
  copyText,
  getUrlParam,
  showCopiedNotification,
} from "../../core/utils.js";
import { state } from "../../core/state.js";
import {
  getPublisherQualityAlerts,
  normalizePublisherProtocolLabel,
} from "../publisher-quality.js";
import {
  getMediaFileAnalysis,
  listMediaFiles,
  startIngest,
  startRecording,
  stopIngest,
  stopRecording,
} from "../../core/api.js";
import type { MediaFile, MediaFileAnalysis } from "../../core/api-types.js";
import type { PipelineView } from "../../types.js";
import {
  pipelineViewDependencies,
  setPipelineViewDependencies,
} from "../pipeline-dependencies.js";
import {
  buildPipelineOperateHeaderModel,
  buildPipelineOperateInputStatusModel,
} from "../pipeline-operate-view-model.js";
import type {
  PipelineOperateFileSourceModel,
} from "../pipeline-operate-view-model.js";
import {
  getPendingFileIngestIntent,
  getPendingRecordingIntent,
  getFileIngestLifecycleError,
  getRecordingLifecycleError,
  setFileIngestLifecycleError,
  setPendingFileIngestIntent,
  setPendingRecordingIntent,
  setRecordingLifecycleError,
} from "./recording.js";
import {
  configurePipelineHeaderPresentation,
  configurePipelineInputStatusPresentation,
  pipelineHeaderPresentationHook,
  pipelineInputStatusPresentationHook,
} from "./config.js";
import {
  buildAudioTrackModels,
  setAudioTrackStateChangeHandler,
} from "./audio.js";
import {
  formatFileContainer,
  formatFileModifiedAt,
  formatFileSize,
  formatSourceDuration,
  formatSourceFps,
  formatSourceGop,
  getFileSourceName,
} from "./file-source.js";

// ── Module-level state ─────────────────────────────────────────────────

const ingestUiState = {
  selectedProtocol: "rtmp",
};

const sourceFileMetadataCache = new Map<string, MediaFile | null>();
const sourceFileAnalysisCache = new Map<string, MediaFileAnalysis | null>();
let sourceFileMetadataLoadPromise: Promise<void> | null = null;
const sourceFileAnalysisLoadPromises = new Map<string, Promise<void>>();
let lastRenderedPipelineInfoId: string | null = null;

// ── Toggle actions ─────────────────────────────────────────────────────

export async function togglePipelineRecording(pipeId: string): Promise<void> {
  const pipe = state.pipelines.find((candidate) => candidate.id === pipeId);
  if (!pipe || getPendingRecordingIntent(pipeId)) return;
  const recordingEnabled = pipe.recording.enabled;
  setRecordingLifecycleError(pipeId, null);
  setPendingRecordingIntent(
    pipeId,
    recordingEnabled ? "stopping" : "starting",
  );
  renderPipelineInfoColumn(pipeId);
  try {
    const res = recordingEnabled
      ? await stopRecording(pipeId)
      : await startRecording(pipeId);
    if (res !== null) {
      pipelineViewDependencies.updateDashboardPipelineRecordingState?.(
        pipeId,
        res,
      );
    } else {
      setRecordingLifecycleError(
        pipeId,
        recordingEnabled
          ? "Stop recording did not complete. Check the error banner and retry when ready."
          : "Start recording did not complete. Check the error banner and retry when ready.",
      );
    }
  } finally {
    setPendingRecordingIntent(pipeId, null);
    renderPipelineInfoColumn(pipeId);
  }
}

export async function togglePipelineFileIngest(pipeId: string): Promise<void> {
  const pipe = state.pipelines.find((candidate) => candidate.id === pipeId);
  const fileIngest = pipe?.fileIngest || null;
  const configured = Boolean(
    pipe &&
      (pipe.inputSource || "").startsWith("file:") &&
      fileIngest?.configured &&
      fileIngest.id,
  );
  if (
    !pipe ||
    !configured ||
    !fileIngest?.id ||
    getPendingFileIngestIntent(pipeId)
  ) {
    return;
  }
  const running = Boolean(fileIngest.running);
  setFileIngestLifecycleError(pipeId, null);
  setPendingFileIngestIntent(pipeId, running ? "stopping" : "starting");
  renderPipelineInfoColumn(pipeId);
  try {
    const res = running
      ? await stopIngest(fileIngest.id)
      : await startIngest(fileIngest.id);
    if (res !== null) {
      pipelineViewDependencies.updateDashboardPipelineFileIngestState?.(
        pipeId,
        {
          configured: true,
          id: res.id,
          filename: res.filename,
          streamKey: res.streamKey,
          loop: res.loop,
          startTime: res.startTime,
          liveOptimized: res.liveOptimized,
          targetGopSeconds: res.targetGopSeconds,
          running: res.running,
        },
      );
      void pipelineViewDependencies.awaitDashboardRuntimeMutationConvergence?.();
    } else {
      setFileIngestLifecycleError(
        pipeId,
        running
          ? "Stop file ingest did not complete. Check the error banner and retry when ready."
          : "Start file ingest did not complete. Check the error banner and retry when ready.",
      );
    }
  } finally {
    setPendingFileIngestIntent(pipeId, null);
    renderPipelineInfoColumn(pipeId);
  }
}

// ── Ingest protocol / copy helpers ─────────────────────────────────────

export function selectPipelineIngestProtocol(
  pipeId: string,
  protocol: "rtmp" | "srt",
): void {
  const pipeline = state.pipelines.find(({ id }) => id === pipeId);
  if (!pipeline?.ingestUrls[protocol]) return;
  ingestUiState.selectedProtocol = protocol;
  renderPipelineInfoColumn(pipeId);
}

export async function copyPipelineStreamKey(pipeId: string): Promise<void> {
  const streamKey = state.pipelines.find(({ id }) => id === pipeId)?.key;
  if (streamKey && (await copyText(streamKey))) showCopiedNotification();
}

export async function copyPipelineIngestUrl(
  pipeId: string,
  protocol: "rtmp" | "srt",
): Promise<void> {
  const url = state.pipelines.find(({ id }) => id === pipeId)?.ingestUrls[
    protocol
  ];
  if (url && (await copyText(url))) showCopiedNotification();
}

// ── Source file cache & loading (keeps cache state in main) ────────────

function rerenderSelectedPipelineIfSourceFileLoaded(
  filename: string | null,
  kind: "metadata" | "analysis",
): void {
  const selectedPipe = getUrlParam("p") || lastRenderedPipelineInfoId;
  if (!selectedPipe) return;
  const selectedPipeline =
    state.pipelines.find((pipe) => pipe.id === selectedPipe) || null;
  if (!selectedPipeline) return;
  const selectedFilename = getFileSourceName(selectedPipeline);
  if (!selectedFilename) return;
  if (filename && selectedFilename !== filename) return;
  const hasLoadedData =
    kind === "metadata"
      ? sourceFileMetadataCache.has(selectedFilename)
      : sourceFileAnalysisCache.has(selectedFilename);
  if (!hasLoadedData) return;
  renderPipelineInfoColumn(selectedPipe);
}

function scheduleSourceFileMetadataLoad(filename: string | null): void {
  if (!filename || sourceFileMetadataCache.has(filename)) return;
  if (typeof fetch !== "function" || sourceFileMetadataLoadPromise) return;

  sourceFileMetadataLoadPromise = listMediaFiles()
    .then((result) => {
      for (const file of result?.files || []) {
        sourceFileMetadataCache.set(file.name, file);
      }
      if (!sourceFileMetadataCache.has(filename)) {
        sourceFileMetadataCache.set(filename, null);
      }
    })
    .catch(() => {
      sourceFileMetadataCache.set(filename, null);
    })
    .finally(() => {
      sourceFileMetadataLoadPromise = null;
      rerenderSelectedPipelineIfSourceFileLoaded(null, "metadata");
    });
}

function scheduleSourceFileAnalysisLoad(filename: string | null): void {
  if (!filename || sourceFileAnalysisCache.has(filename)) return;
  if (typeof fetch !== "function") return;
  if (sourceFileAnalysisLoadPromises.has(filename)) return;

  const request = getMediaFileAnalysis(filename)
    .then((analysis) => {
      sourceFileAnalysisCache.set(filename, analysis);
    })
    .catch(() => {
      sourceFileAnalysisCache.set(filename, null);
    })
    .finally(() => {
      sourceFileAnalysisLoadPromises.delete(filename);
      rerenderSelectedPipelineIfSourceFileLoaded(filename, "analysis");
    });
  sourceFileAnalysisLoadPromises.set(filename, request);
}

// ── Main pipeline info column renderer ─────────────────────────────────

export function renderPipelineInfoColumn(selectedPipe: string | null): void {
  lastRenderedPipelineInfoId = selectedPipe;
  setAudioTrackStateChangeHandler(() => renderPipelineInfoColumn(selectedPipe));
  if (!selectedPipe) {
    pipelineHeaderPresentationHook?.(null);
    pipelineInputStatusPresentationHook?.(null);
    document.getElementById("pipe-info-col")?.classList.add("hidden");
    return;
  }

  document.getElementById("pipe-info-col")?.classList.remove("hidden");

  const pipe = state.pipelines.find((p) => p.id === selectedPipe);
  if (!pipe) {
    pipelineHeaderPresentationHook?.(null);
    pipelineInputStatusPresentationHook?.(null);
    console.error("Pipeline not found:", selectedPipe);
    return;
  }
  pipelineHeaderPresentationHook?.(
    buildPipelineOperateHeaderModel(state.pipelines, selectedPipe, {
      recordingIntent: getPendingRecordingIntent(pipe.id),
      fileIngestIntent: getPendingFileIngestIntent(pipe.id),
      recordingError: getRecordingLifecycleError(pipe.id),
      fileIngestError: getFileIngestLifecycleError(pipe.id),
    }),
  );
  const isFileSource = (pipe.inputSource || "").startsWith("file:");
  const fileSourceName = getFileSourceName(pipe);
  const cachedSourceFile = fileSourceName
    ? sourceFileMetadataCache.get(fileSourceName) || null
    : null;
  const cachedSourceAnalysis = fileSourceName
    ? sourceFileAnalysisCache.get(fileSourceName) || null
    : null;
  if (isFileSource) {
    scheduleSourceFileMetadataLoad(fileSourceName);
    scheduleSourceFileAnalysisLoad(fileSourceName);
  }
  const targetGopSeconds = pipe.fileIngest?.targetGopSeconds || 2;
  const sparseSource =
    Number(cachedSourceAnalysis?.maxKeyframeIntervalSec ?? 0) >
    targetGopSeconds;
  const fileSourceModel: PipelineOperateFileSourceModel | null = isFileSource
    ? {
        filename: fileSourceName || "--",
        details: [
          {
            key: "container",
            label: "Container",
            value: formatFileContainer(fileSourceName),
          },
          {
            key: "size",
            label: "Size",
            value: formatFileSize(
              cachedSourceFile?.sourceSize ?? cachedSourceFile?.size ?? null,
            ),
          },
          {
            key: "modified",
            label: "Modified",
            value: formatFileModifiedAt(cachedSourceFile?.modifiedAt || null),
          },
          {
            key: "loop",
            label: "Loop",
            value: pipe.fileIngest?.configured
              ? pipe.fileIngest.loop
                ? "Enabled"
                : "Disabled"
              : "--",
          },
          {
            key: "start",
            label: "Start offset",
            value: pipe.fileIngest?.configured
              ? pipe.fileIngest.startTime || "00:00:00"
              : "--",
          },
          {
            key: "optimization",
            label: "Live optimized",
            value: pipe.fileIngest?.configured
              ? pipe.fileIngest.liveOptimized
                ? `Enabled (${targetGopSeconds}s GOP)`
                : "Disabled"
              : "--",
          },
          {
            key: "codec",
            label: "Video codec",
            value: cachedSourceAnalysis?.videoCodec?.toUpperCase() || "--",
          },
          {
            key: "fps",
            label: "Frame rate",
            value: formatSourceFps(cachedSourceAnalysis?.fps),
          },
          {
            key: "duration",
            label: "Duration",
            value: formatSourceDuration(cachedSourceAnalysis?.durationSec),
          },
          {
            key: "gop",
            label: "GOP",
            value: formatSourceGop(cachedSourceAnalysis),
          },
        ],
        warning: sparseSource
          ? pipe.fileIngest?.liveOptimized
            ? `Sparse source GOP detected: max ${Number(cachedSourceAnalysis?.maxKeyframeIntervalSec).toFixed(1)}s. Live Optimized is targeting ${targetGopSeconds}s keyframes.`
            : `Sparse source GOP detected: max ${Number(cachedSourceAnalysis?.maxKeyframeIntervalSec).toFixed(1)}s exceeds the ${targetGopSeconds}s live target.`
          : null,
      }
    : null;
  pipelineInputStatusPresentationHook?.(
    buildPipelineOperateInputStatusModel(
      state.pipelines,
      selectedPipe,
      ingestUiState.selectedProtocol as "rtmp" | "srt",
      fileSourceModel,
      buildAudioTrackModels(pipe.id, pipe.input.audioTracks || []),
    ),
  );
}

// ── Re-exports ─────────────────────────────────────────────────────────
export { setPipelineViewDependencies } from "../pipeline-dependencies.js";
export { renderOutsColumn } from "../pipeline-output-list.js";

// Backward-compatible re-exports from extracted sub-modules
export {
  editPipelineAudioTrack,
  updatePipelineAudioTrackDraft,
  cancelPipelineAudioTrackEdit,
  savePipelineAudioTrack,
  mountPipelineInputPreview,
  clearPipelineInputPreview,
} from "./audio.js";
export {
  configurePipelineHeaderPresentation,
  configurePipelineInputStatusPresentation,
} from "./config.js";
