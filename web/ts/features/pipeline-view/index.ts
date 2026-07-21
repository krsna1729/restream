import {
  copyText,
  escapeHtml,
  formatCodecName,
  formatMaskedStreamKey,
  getUrlParam,
  msToHHMMSS,
  showCopiedNotification,
} from "../../core/utils.js";
import { setBitrateWithSubtleUnit } from "../metric-format.js";
import { state } from "../../core/state.js";
import {
  getPublisherQualityAlerts,
  normalizePublisherProtocolLabel,
} from "../publisher-quality.js";
import {
  parseProtocolAwareIngestUrl,
  renderProtocolDetails,
} from "../ingest-url-details.js";
import { clearInputPreview, renderInputPreview } from "../input-preview.js";
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
  legacyPipelineAudioTracksRenderEnabled,
  legacyPipelineInputStatusRenderEnabled,
  legacyPipelinePreviewRenderEnabled,
  pipelineHeaderPresentationHook,
  pipelineInputStatusPresentationHook,
} from "./config.js";
import {
  buildAudioTrackModels,
  formatShortDurationMs,
  renderAudioTracksTable,
} from "./audio.js";
import {
  formatFileContainer,
  formatFileModifiedAt,
  formatFileSize,
  formatSourceDuration,
  formatSourceFps,
  formatSourceGop,
  getFileSourceName,
  hideFileIngestControl,
  setTextIfPresent,
} from "./file-source.js";
import { setTextIfChanged, syncPublisherMeta } from "./publisher.js";

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

// ── Video track details ────────────────────────────────────────────────

function renderVideoTrackDetails(
  video: Partial<NonNullable<PipelineView["input"]["video"]>>,
  selection: PipelineView["input"]["videoTrackSelection"] | null | undefined,
): void {
  const pidStat = document.getElementById("input-video-pid-stat");
  const pidValue = document.getElementById("input-video-pid");
  const hasPid = Number.isFinite(video.pid as number);
  pidStat?.classList.toggle("hidden", !hasPid);
  if (pidValue) {
    setTextIfChanged(
      pidValue,
      hasPid ? `0x${Number(video.pid).toString(16).toUpperCase()}` : "",
    );
  }

  const selectionStat = document.getElementById("input-video-selection-stat");
  const selectionValue = document.getElementById("input-video-selection");
  const availableTrackCount = Number(selection?.availableTrackCount || 0);
  const selectedTrackIndex =
    typeof selection?.selectedTrackIndex === "number"
      ? selection.selectedTrackIndex
      : null;
  const showSelection = availableTrackCount > 1 && selectedTrackIndex !== null;
  selectionStat?.classList.toggle("hidden", !showSelection);
  if (selectionValue) {
    setTextIfChanged(
      selectionValue,
      showSelection
        ? `Track ${selectedTrackIndex + 1} of ${availableTrackCount}`
        : "",
    );
  }
}

// ── Main pipeline info column renderer ─────────────────────────────────

export function renderPipelineInfoColumn(selectedPipe: string | null): void {
  lastRenderedPipelineInfoId = selectedPipe;
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

  const pipeNameEl = document.getElementById("pipe-name");
  if (pipeNameEl) pipeNameEl.textContent = pipe.name;

  const historyBtn = document.getElementById("pipe-history-btn");
  if (historyBtn) {
    historyBtn.onclick = () => {
      pipelineViewDependencies.openPipelineHistoryModal?.(pipe.id, pipe.name);
    };
  }

  const recordBtn = document.getElementById(
    "record-pipe-btn",
  ) as HTMLButtonElement | null;
  if (recordBtn) {
    const isRecordingEnabled = pipe.recording.enabled;
    const inputOn = pipe.input.status === "on";
    const canStart = inputOn || isRecordingEnabled;
    const pendingIntent = getPendingRecordingIntent(pipe.id);
    const pending = pendingIntent !== null;
    recordBtn.textContent = pending
      ? pendingIntent === "starting"
        ? "Starting..."
        : "Stopping..."
      : isRecordingEnabled
        ? "Stop Rec"
        : "Record";
    recordBtn.classList.toggle(
      "btn-error",
      pendingIntent === "stopping" || (!pending && isRecordingEnabled),
    );
    recordBtn.classList.toggle(
      "btn-accent",
      pendingIntent === "starting" || (!pending && !isRecordingEnabled),
    );
    recordBtn.classList.toggle(
      "btn-outline",
      pendingIntent !== "starting" && !isRecordingEnabled,
    );
    recordBtn.disabled = pending || !canStart;
    recordBtn.classList.toggle("btn-disabled", pending || !canStart);
    recordBtn.title = pending
      ? ""
      : !canStart
        ? "Input must be on to start recording"
        : "";
    recordBtn.onclick = () => togglePipelineRecording(pipe.id);
  }

  const fileIngestBtn = document.getElementById(
    "file-ingest-pipe-btn",
  ) as HTMLButtonElement | null;
  if (fileIngestBtn) {
    const fileIngest = pipe.fileIngest || null;
    const configured = Boolean(isFileSource && fileIngest?.configured);
    if (!configured || !fileIngest?.id) {
      setPendingFileIngestIntent(pipe.id, null);
      hideFileIngestControl(fileIngestBtn);
    } else {
      const running = Boolean(fileIngest.running);
      const pendingIntent = getPendingFileIngestIntent(pipe.id);
      const pending = pendingIntent !== null;
      fileIngestBtn.classList.remove("hidden");
      fileIngestBtn.textContent = pending
        ? pendingIntent === "starting"
          ? "Starting File..."
          : "Stopping File..."
        : running
          ? "Stop File"
          : "Start File";
      fileIngestBtn.classList.toggle(
        "btn-error",
        pendingIntent === "stopping" || (!pending && running),
      );
      fileIngestBtn.classList.toggle(
        "btn-accent",
        pendingIntent === "starting" || (!pending && !running),
      );
      fileIngestBtn.classList.toggle(
        "btn-outline",
        pendingIntent !== "starting" && !running,
      );
      fileIngestBtn.disabled = pending;
      fileIngestBtn.classList.toggle("btn-disabled", pending);
      fileIngestBtn.title = fileIngest.filename
        ? `${running ? "Stop" : "Start"} file ingest for ${fileIngest.filename}`
        : "";
      fileIngestBtn.onclick = () => togglePipelineFileIngest(pipe.id);
    }
  }

  const graphBtn = document.getElementById(
    "graph-pipe-btn",
  ) as HTMLButtonElement | null;
  if (graphBtn) {
    graphBtn.disabled = false;
    graphBtn.classList.remove("btn-disabled");
    graphBtn.title = "";
    graphBtn.onclick = () => {
      pipelineViewDependencies.openGraphExplorer?.(pipe.id);
    };
  }

  const diagnoseBtn = document.getElementById(
    "diagnose-pipe-btn",
  ) as HTMLButtonElement | null;
  if (diagnoseBtn) {
    const inputOn = pipe.input.status === "on";
    diagnoseBtn.disabled = !inputOn;
    diagnoseBtn.classList.toggle("btn-disabled", !inputOn);
    diagnoseBtn.title = inputOn
      ? ""
      : "Input must be online to run diagnostics";
    diagnoseBtn.onclick = () => {
      pipelineViewDependencies.openDiagnosticsModal?.(pipe.id);
    };
  }

  const editPipeBtn = document.getElementById(
    "edit-pipe-btn",
  ) as HTMLButtonElement | null;
  if (editPipeBtn) {
    const isRecordingActive = pipe.recording.active;
    editPipeBtn.disabled = isRecordingActive;
    editPipeBtn.classList.toggle("btn-disabled", isRecordingActive);
    editPipeBtn.title = isRecordingActive
      ? "Stop recording before editing"
      : "";
  }
  const inputTimeElem = document.getElementById("input-time");
  if (inputTimeElem) {
    inputTimeElem.classList.add("hidden");
    inputTimeElem.textContent =
      pipe.input.time === null ? "" : msToHHMMSS(pipe.input.time);
  }

  const deletePipeBtn = document.getElementById("delete-pipe-btn");
  if (deletePipeBtn) {
    if (pipe.outs.find((o) => o.status !== "off")) {
      deletePipeBtn.classList.add("btn-disabled");
      deletePipeBtn.title = "Stop all outputs before deleting the pipeline";
    } else {
      deletePipeBtn.classList.remove("btn-disabled");
      deletePipeBtn.title = "";
    }
  }

  const streamKeySection = document.getElementById("stream-key-section");
  streamKeySection?.classList.toggle("hidden", isFileSource);
  const fileSourceSection = document.getElementById("file-source-section");
  fileSourceSection?.classList.toggle("hidden", !isFileSource);
  const fileSourceInline = document.getElementById("file-source-inline");
  if (fileSourceInline) {
    fileSourceInline.textContent = fileSourceName || "--";
    fileSourceInline.title = fileSourceName || "";
  }
  const fileSourceDetails = document.getElementById("file-source-details");
  fileSourceDetails?.classList.toggle("hidden", !isFileSource);
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
  setTextIfPresent(
    "file-source-container",
    formatFileContainer(fileSourceName || pipe.fileIngest?.filename || null),
  );
  setTextIfPresent(
    "file-source-size",
    formatFileSize(
      cachedSourceFile?.sourceSize ?? cachedSourceFile?.size ?? null,
    ),
  );
  setTextIfPresent(
    "file-source-modified",
    formatFileModifiedAt(cachedSourceFile?.modifiedAt || null),
  );
  setTextIfPresent(
    "file-source-loop",
    pipe.fileIngest?.configured
      ? pipe.fileIngest.loop
        ? "Enabled"
        : "Disabled"
      : "--",
  );
  setTextIfPresent(
    "file-source-start-time",
    pipe.fileIngest?.configured
      ? pipe.fileIngest.startTime || "00:00:00"
      : "--",
  );
  setTextIfPresent(
    "file-source-optimization",
    pipe.fileIngest?.configured
      ? pipe.fileIngest.liveOptimized
        ? `Enabled (${pipe.fileIngest.targetGopSeconds || 2}s GOP)`
        : "Disabled"
      : "--",
  );
  setTextIfPresent(
    "file-source-video-codec",
    cachedSourceAnalysis?.videoCodec
      ? cachedSourceAnalysis.videoCodec.toUpperCase()
      : "--",
  );
  setTextIfPresent(
    "file-source-fps",
    formatSourceFps(cachedSourceAnalysis?.fps),
  );
  setTextIfPresent(
    "file-source-duration",
    formatSourceDuration(cachedSourceAnalysis?.durationSec),
  );
  setTextIfPresent("file-source-gop", formatSourceGop(cachedSourceAnalysis));
  const fileSourceWarning = document.getElementById("file-source-gop-warning");
  if (fileSourceWarning) {
    if (isFileSource && sparseSource) {
      fileSourceWarning.textContent = pipe.fileIngest?.liveOptimized
        ? `Sparse source GOP detected: max ${Number(cachedSourceAnalysis?.maxKeyframeIntervalSec).toFixed(1)}s. Live Optimized is targeting ${targetGopSeconds}s keyframes.`
        : `Sparse source GOP detected: max ${Number(cachedSourceAnalysis?.maxKeyframeIntervalSec).toFixed(1)}s exceeds the ${targetGopSeconds}s live target.`;
      fileSourceWarning.classList.remove("hidden");
    } else {
      fileSourceWarning.classList.add("hidden");
      fileSourceWarning.textContent = "";
    }
  }

  const streamKey = pipe.key;
  const streamKeyInline = document.getElementById("stream-key-inline");
  const streamKeyCopyBtn = document.getElementById(
    "stream-key-copy-btn",
  ) as HTMLButtonElement | null;
  if (streamKeyInline && !isFileSource) {
    streamKeyInline.dataset.copy = streamKey ?? "";
    streamKeyInline.textContent = formatMaskedStreamKey(streamKey);
    streamKeyInline.title = "";
  }
  if (streamKeyCopyBtn) {
    streamKeyCopyBtn.disabled = isFileSource;
    streamKeyCopyBtn.classList.toggle("btn-disabled", isFileSource);
    streamKeyCopyBtn.onclick = isFileSource
      ? null
      : async () => {
          if (streamKey && (await copyText(streamKey)))
            showCopiedNotification();
        };
  }

  const ingestUrls = pipe.ingestUrls || {};
  const availableProtocols = (["rtmp", "srt"] as const).filter((protocol) => {
    const url = ingestUrls[protocol];
    return typeof url === "string" && url.trim() !== "";
  });

  if (
    !availableProtocols.includes(
      ingestUiState.selectedProtocol as "rtmp" | "srt",
    )
  ) {
    ingestUiState.selectedProtocol = availableProtocols[0] || "rtmp";
  }

  (["rtmp", "srt"] as const).forEach((protocol) => {
    const btn = document.getElementById(`ingest-protocol-${protocol}`);
    if (!btn) return;

    const isAvailable = availableProtocols.includes(protocol);
    const isActive = ingestUiState.selectedProtocol === protocol;

    btn.toggleAttribute("disabled", !isAvailable);
    btn.classList.toggle("btn-disabled", !isAvailable);
    btn.classList.remove(
      "border-accent/35",
      "bg-accent/18",
      "text-accent",
      "border-base-content/10",
      "bg-base-100/70",
      "text-base-content/80",
      "opacity-60",
    );
    if (isActive && isAvailable) {
      btn.classList.add("border-accent/35", "bg-accent/18", "text-accent");
    } else {
      btn.classList.add(
        "border-base-content/10",
        "bg-base-100/70",
        "text-base-content/80",
      );
    }
    if (!isAvailable) {
      btn.classList.add("opacity-60");
    }
    btn.setAttribute("aria-pressed", isActive ? "true" : "false");
    btn.onclick = () => {
      if (!isAvailable) return;
      ingestUiState.selectedProtocol = protocol;
      renderPipelineInfoColumn(selectedPipe);
    };
  });

  const selectedProtocol = ingestUiState.selectedProtocol;
  const selectedUrl =
    (ingestUrls as unknown as Record<string, string | null>)[
      selectedProtocol
    ] || "";

  const ingestUrlSection = document.getElementById("ingest-url-section");
  if (ingestUrlSection) {
    ingestUrlSection.classList.toggle(
      "hidden",
      isFileSource || availableProtocols.length === 0,
    );
  }

  const maskedUrl = streamKey
    ? selectedUrl.replace(streamKey, formatMaskedStreamKey(streamKey))
    : selectedUrl;

  const ingestUrlValue = document.getElementById("ingest-url");
  const ingestUrlSurface = document.getElementById("ingest-url-surface");
  if (ingestUrlValue) {
    ingestUrlValue.dataset.copy = isFileSource ? "" : selectedUrl;
    ingestUrlValue.textContent = isFileSource ? "" : maskedUrl || "--";
  }
  if (ingestUrlSurface) {
    ingestUrlSurface.classList.toggle("hidden", isFileSource || !selectedUrl);
  }

  const ingestUrlCopyBtn = document.getElementById(
    "ingest-url-copy-btn",
  ) as HTMLButtonElement | null;
  if (ingestUrlCopyBtn) {
    ingestUrlCopyBtn.disabled = isFileSource || !selectedUrl;
    ingestUrlCopyBtn.classList.toggle(
      "btn-disabled",
      isFileSource || !selectedUrl,
    );
    ingestUrlCopyBtn.onclick = async () => {
      if (isFileSource || !selectedUrl) return;
      if (await copyText(selectedUrl)) showCopiedNotification();
    };
  }

  const ingestUrlDetails = document.getElementById("ingest-url-details");
  const ingestDetailsGrid = document.getElementById(
    "ingest-details-grid",
  ) as HTMLElement | null;
  const parsedIngestDetails = parseProtocolAwareIngestUrl(
    selectedProtocol,
    selectedUrl,
  );
  if (ingestUrlDetails) {
    ingestUrlDetails.classList.toggle(
      "hidden",
      isFileSource || !selectedUrl || !parsedIngestDetails,
    );
  }
  renderProtocolDetails(
    ingestDetailsGrid,
    selectedProtocol,
    parsedIngestDetails,
  );

  const playerElem = document.getElementById(
    "video-player",
  ) as HTMLElement | null;
  const inputStatsElem = document.getElementById("input-stats");
  if (pipe.input.status === "off") {
    playerElem?.classList.add("hidden");
    inputStatsElem?.classList.add("hidden");
    clearInputPreview(playerElem);
  } else {
    playerElem?.classList.toggle("hidden", !legacyPipelinePreviewRenderEnabled);
    inputStatsElem?.classList.remove("hidden");
    if (legacyPipelinePreviewRenderEnabled) {
      renderInputPreview(playerElem, pipe);
    } else {
      clearInputPreview(playerElem);
    }

    const video = pipe.input.video || {};
    const stats =
      pipe.stats || ({} as Partial<import("../../types.js").PipelineStats>);

    const setTextContent = (id: string, value: unknown): void => {
      const el = document.getElementById(id);
      if (el) setTextIfChanged(el, String(value ?? "--"));
    };

    setTextContent("input-video-codec", formatCodecName(video.codec) || "--");
    setTextContent(
      "input-video-resolution",
      video.width && video.height ? `${video.width}x${video.height}` : "--",
    );
    setTextContent(
      "input-video-fps",
      video.fps !== null && video.fps !== undefined ? video.fps : "--",
    );
    setTextContent("input-video-level", video.level || "--");
    setTextContent("input-video-profile", video.profile || "--");
    renderVideoTrackDetails(video, pipe.input.videoTrackSelection);

    if (legacyPipelineAudioTracksRenderEnabled) {
      renderAudioTracksTable(pipe.id, pipe.input.audioTracks || []);
    }

    setBitrateWithSubtleUnit("input-total-bw", stats.inputBitrateKbps);
    setBitrateWithSubtleUnit("output-total-bw", stats.outputBitrateKbps);
    setTextContent(
      "input-reader-count",
      stats.readerCount !== null && stats.readerCount !== undefined
        ? stats.readerCount
        : "--",
    );
    setTextContent(
      "input-output-count",
      stats.outputCount !== null && stats.outputCount !== undefined
        ? stats.outputCount
        : "--",
    );
  }

  let publisherMeta = document.getElementById("publisher-meta");
  if (!publisherMeta) {
    publisherMeta = document.createElement("div");
    publisherMeta.id = "publisher-meta";
    publisherMeta.className = "mt-1 mb-4 flex flex-wrap items-center gap-2";
    inputStatsElem?.parentNode?.insertBefore(publisherMeta, inputStatsElem);
  }
  publisherMeta.hidden = !legacyPipelineInputStatusRenderEnabled;

  const publisher = pipe.input.publisher;
  const qualityAlerts = publisher ? getPublisherQualityAlerts(publisher) : [];
  const isHealthy = qualityAlerts.length === 0;
  const unexpectedCount = pipe.input.unexpectedReadersCount || 0;
  const hlsPreview = pipe.hlsPreview;
  const lastDisconnectTitle = [
    pipe.input.lastSessionProtocol
      ? `protocol=${pipe.input.lastSessionProtocol}`
      : "",
    pipe.input.lastFailurePhase ? `phase=${pipe.input.lastFailurePhase}` : "",
    pipe.input.lastDisconnectReason || "",
    pipe.input.lastRemoteAddr ? `remote=${pipe.input.lastRemoteAddr}` : "",
    Number.isFinite(pipe.input.lastSessionBytesReceived as number)
      ? `bytes=${pipe.input.lastSessionBytesReceived}`
      : "",
    pipe.input.lastDisconnectAgeMs !== null
      ? `age=${formatShortDurationMs(pipe.input.lastDisconnectAgeMs)} ago`
      : "",
  ]
    .filter(Boolean)
    .join(" ");
  const hlsPreviewTitle = [
    hlsPreview.active
      ? "Browser preview segmenter is active."
      : "Browser preview segmenter is idle.",
    `segments=${hlsPreview.segments}`,
    `playlistBytes=${hlsPreview.playlistBytes}`,
    `persistentConsumers=${hlsPreview.persistentConsumers}`,
    `lastAccess=${formatShortDurationMs(hlsPreview.lastAccessAgeMs)} ago`,
  ].join(" ");
  syncPublisherMeta(
    publisherMeta as HTMLElement,
    [
      pipe.input.time !== null
        ? {
            key: "uptime",
            tagName: "span",
            className: "badge text-sm px-3",
            text: msToHHMMSS(pipe.input.time) || "--",
            title: "",
          }
        : null,
      pipe.input.status === "on" && !pipe.input.probeReady
        ? {
            key: "probe",
            tagName: "span",
            className: "badge badge-warning text-sm px-3",
            text: "Probing",
            title: `Waiting for stream metadata${pipe.input.probePendingMs ? ` (${(pipe.input.probePendingMs / 1000).toFixed(1)}s)` : ""}`,
          }
        : null,
      publisher
        ? {
            key: "protocol",
            tagName: "span",
            className: "badge badge-info text-sm px-3",
            text: normalizePublisherProtocolLabel(publisher.protocol),
            title: "",
          }
        : null,
      publisher?.remoteAddr
        ? {
            key: "remote",
            tagName: "span",
            className: "badge badge-outline font-mono text-sm px-3",
            text: publisher.remoteAddr,
            title: "",
          }
        : null,
      publisher
        ? {
            key: "quality",
            tagName: "button",
            className: `badge text-sm px-3 cursor-pointer ${isHealthy ? "badge-success" : "badge-warning"}`,
            text: isHealthy ? "Healthy" : "Unhealthy",
            title: qualityAlerts.length
              ? qualityAlerts.map((alert) => alert.label).join("\n")
              : "Open publisher health details",
          }
        : null,
      pipe.input.status === "off" && pipe.input.lastDisconnectAt
        ? {
            key: "disconnect",
            tagName: "span",
            className: `badge ${pipe.input.recentDisconnectError ? "badge-warning" : "badge-outline"} text-sm px-3`,
            text: pipe.input.recentDisconnectError
              ? "Last failure"
              : "Last disconnect",
            title: escapeHtml(
              lastDisconnectTitle || "Recent ingest disconnect",
            ),
          }
        : null,
      hlsPreview.active ||
      hlsPreview.segments > 0 ||
      hlsPreview.persistentConsumers > 0
        ? {
            key: "preview",
            tagName: "span",
            className: `badge ${hlsPreview.active ? "badge-success" : "badge-outline"} text-sm px-3`,
            text: hlsPreview.active ? "Preview live" : "Preview idle",
            title: escapeHtml(hlsPreviewTitle),
          }
        : null,
      unexpectedCount > 0
        ? {
            key: "unexpected",
            tagName: "span",
            className: "badge badge-sm badge-error",
            text: `${unexpectedCount} unexpected reader${unexpectedCount === 1 ? "" : "s"}`,
            title: "",
          }
        : null,
    ].filter(Boolean) as {
      key: string;
      tagName: "span" | "button";
      className: string;
      text: string;
      title: string;
    }[],
    pipe.id,
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
