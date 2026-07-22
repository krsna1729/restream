import {
  getStreamKeys, createPipeline, updatePipeline, deletePipeline,
  listMediaFiles, getPipelineFileIngest, getMediaFileAnalysis,
} from "../../core/api.js";
import type {
  MediaFile, MediaFileAnalysis, PipelineFileIngestConfig,
} from "../../core/api.js";
import {
  getUrlParam, setUrlParam, escapeHtml, showErrorAlert, confirmInApp,
  formatMaskedStreamKey,
} from "../../core/utils.js";
import { state } from "../../core/state.js";
import { isOutputManagedActive } from "../../core/output-status.js";
import {
  upsertDashboardPipelineConfig, removeDashboardPipelineConfig,
} from "../dashboard.js";
import type {
  ConfigPipeline, PipelineView, StreamKey, SrtPipelineIngestConfig,
} from "../../types.js";

const DEFAULT_FILE_INGEST_GOP_SECONDS = 2;
const fileAnalysisCache = new Map<string, MediaFileAnalysis | null>();
let pendingFileAnalysisRequest = 0;
type PipeModalMode = "create" | "edit";

let currentPipeModalMode: PipeModalMode = "edit";
let currentPipeModalPipeline: PipelineView | null = null;
const BUILT_IN_PROFILE_ORDER = ["h264", "720p", "1080p"];

function orderedTranscodeProfileNames(): string[] {
  const names = Array.from(
    new Set([
      ...BUILT_IN_PROFILE_ORDER,
      ...Object.keys(state.config?.transcodeProfiles || {}),
    ]),
  );
  return names.sort((a, b) => {
    const ai = BUILT_IN_PROFILE_ORDER.indexOf(a);
    const bi = BUILT_IN_PROFILE_ORDER.indexOf(b);
    if (ai !== -1 || bi !== -1) {
      if (ai === -1) return 1;
      if (bi === -1) return -1;
      return ai - bi;
    }
    return a.localeCompare(b);
  });
}

export function populateOutputEncodingSelect(selectedEncoding = "source"): void {
  const select = document.getElementById(
    "out-encoding-input",
  ) as HTMLSelectElement | null;
  if (!select) return;

  const selectedVideoEncoding = selectedEncoding.includes("+")
    ? selectedEncoding.split("+")[0].trim()
    : selectedEncoding.trim();
  const profileNames = orderedTranscodeProfileNames();
  const optionValues = ["source", ...profileNames];
  if (
    selectedVideoEncoding &&
    !optionValues.includes(selectedVideoEncoding) &&
    !/^(atrack|downmix|remap):/.test(selectedVideoEncoding.toLowerCase())
  ) {
    optionValues.push(selectedVideoEncoding);
  }

  select.innerHTML = optionValues
    .map((value) => {
      const label =
        value === selectedVideoEncoding &&
        !profileNames.includes(value) &&
        value !== "source"
          ? `${value} (current)`
          : value;
      return `<option value="${escapeHtml(value)}">${escapeHtml(label)}</option>`;
    })
    .join("");
  select.value = optionValues.includes(selectedVideoEncoding)
    ? selectedVideoEncoding
    : "source";
}

export function getSuggestedPipelineName(): string {
  const numbers = state.pipelines
    .filter((p) => p.name.startsWith("Pipeline "))
    .map((p) => parseInt(p.name.split(" ")[1], 10))
    .filter((n) => Number.isFinite(n));
  const nextNumber = Math.max(...numbers, 0) + 1;
  return `Pipeline ${nextNumber}`;
}

async function populatePipelineKeySelect(selectedKey = ""): Promise<string> {
  const keySelect = document.getElementById(
    "pipe-stream-key-input",
  ) as HTMLSelectElement | null;
  if (!keySelect) return selectedKey;
  const keys = await loadStreamKeysOnce();
  const options = selectedKey
    ? keys.filter((key) => key.key === selectedKey)
    : [];
  if (selectedKey && !options.some((key) => key.key === selectedKey)) {
    options.push({ key: selectedKey });
  }

  keySelect.innerHTML = [
    `<option value=""${selectedKey ? "" : " selected"}>Generate new key</option>`,
    ...options.map((key) => {
      const label = key.label
        ? `${key.label} - ${formatMaskedStreamKey(key.key)}`
        : formatMaskedStreamKey(key.key);
      return `<option value="${escapeHtml(key.key)}"${key.key === selectedKey ? " selected" : ""}>${escapeHtml(label)}</option>`;
    }),
  ].join("");
  keySelect.value = selectedKey;
  return selectedKey;
}

let streamKeysCache: StreamKey[] | null = null;
let streamKeysRequest: Promise<StreamKey[]> | null = null;

export async function loadStreamKeysOnce(): Promise<StreamKey[]> {
  if (streamKeysCache) return streamKeysCache;
  if (!streamKeysRequest) {
    streamKeysRequest = getStreamKeys().then((keys) => {
      if (!Array.isArray(keys)) {
        streamKeysRequest = null;
        return [];
      }
      streamKeysCache = keys;
      return streamKeysCache;
    });
  }
  return streamKeysRequest;
}

function filenameFromInputSource(
  inputSource: string | null | undefined,
): string {
  const source = (inputSource || "").trim();
  if (!source) return "";
  return source.startsWith("file:") ? source.slice("file:".length) : source;
}

function setPipeSourceUi(sourceType: "publisher" | "file"): void {
  const sourceSelect = document.getElementById(
    "pipe-source-type-input",
  ) as HTMLSelectElement | null;
  const fileFields = document.getElementById("pipe-file-fields");
  const srtIngestFields = document.getElementById(
    "pipe-srt-ingest-fields",
  ) as HTMLDetailsElement | null;
  if (sourceSelect) sourceSelect.value = sourceType;
  fileFields?.classList.toggle("hidden", sourceType !== "file");
  srtIngestFields?.classList.toggle("hidden", sourceType !== "publisher");
  if (sourceType !== "publisher" && srtIngestFields) {
    srtIngestFields.open = false;
  }
  if (sourceType !== "file") {
    const summary = document.getElementById("pipe-file-analysis-summary");
    const warning = document.getElementById("pipe-file-warning");
    if (summary) summary.textContent = "";
    if (warning) {
      warning.classList.add("hidden");
      warning.textContent = "";
    }
  }
}

function setPipeFileOptimizationUi(liveOptimized: boolean): void {
  const gopInput = document.getElementById(
    "pipe-file-gop-seconds-input",
  ) as HTMLInputElement | null;
  if (!gopInput) return;
  gopInput.disabled = !liveOptimized;
  gopInput.classList.toggle("input-disabled", !liveOptimized);
}

function describePipeFileAnalysis(analysis: MediaFileAnalysis | null): string {
  if (!analysis) return "Could not analyze the selected file yet.";
  if (!analysis.videoCodec)
    return "No video stream detected in the selected file.";
  const parts = [analysis.videoCodec.toUpperCase()];
  if (Number.isFinite(analysis.fps as number)) {
    const fps = Number(analysis.fps);
    parts.push(`${fps.toFixed(fps === Math.round(fps) ? 0 : 1)} FPS`);
  }
  if (Number.isFinite(analysis.durationSec as number)) {
    parts.push(`${Number(analysis.durationSec).toFixed(1)}s`);
  }
  if (Number.isFinite(analysis.averageKeyframeIntervalSec as number)) {
    parts.push(
      `GOP avg ${Number(analysis.averageKeyframeIntervalSec).toFixed(1)}s`,
    );
  }
  if (Number.isFinite(analysis.maxKeyframeIntervalSec as number)) {
    parts.push(`max ${Number(analysis.maxKeyframeIntervalSec).toFixed(1)}s`);
  }
  return parts.join(" | ");
}

function renderPipeFileAnalysis(
  filename: string,
  analysis: MediaFileAnalysis | null,
): void {
  const summary = document.getElementById("pipe-file-analysis-summary");
  const warning = document.getElementById("pipe-file-warning");
  if (summary) {
    summary.textContent = filename ? describePipeFileAnalysis(analysis) : "";
  }
  if (!warning) return;

  const liveOptimized =
    (
      document.getElementById(
        "pipe-file-live-optimized-input",
      ) as HTMLInputElement | null
    )?.checked ?? false;
  const targetGopSeconds = Math.max(
    Number(
      (
        document.getElementById(
          "pipe-file-gop-seconds-input",
        ) as HTMLInputElement | null
      )?.value || DEFAULT_FILE_INGEST_GOP_SECONDS,
    ) || DEFAULT_FILE_INGEST_GOP_SECONDS,
    1,
  );

  const sparse =
    Number(analysis?.maxKeyframeIntervalSec ?? 0) > targetGopSeconds;
  if (!filename || !analysis?.videoCodec || !sparse) {
    warning.classList.add("hidden");
    warning.textContent = "";
    return;
  }

  warning.textContent = liveOptimized
    ? `Sparse source GOP detected: max ${Number(analysis.maxKeyframeIntervalSec).toFixed(1)}s. Live Optimized will re-encode toward a ${targetGopSeconds}s GOP.`
    : `Sparse source GOP detected: max ${Number(analysis.maxKeyframeIntervalSec).toFixed(1)}s exceeds the ${targetGopSeconds}s live target. Enable Live Optimized for steadier preview and recording.`;
  warning.classList.remove("hidden");
}

async function refreshPipeFileAnalysis(selectedFilename = ""): Promise<void> {
  const sourceType =
    (
      document.getElementById(
        "pipe-source-type-input",
      ) as HTMLSelectElement | null
    )?.value === "file"
      ? "file"
      : "publisher";
  if (sourceType !== "file") return;

  const fileSelect = document.getElementById(
    "pipe-file-input",
  ) as HTMLSelectElement | null;
  const filename = selectedFilename || fileSelect?.value?.trim() || "";
  if (!filename) {
    renderPipeFileAnalysis("", null);
    return;
  }

  if (fileAnalysisCache.has(filename)) {
    renderPipeFileAnalysis(filename, fileAnalysisCache.get(filename) || null);
    return;
  }

  const summary = document.getElementById("pipe-file-analysis-summary");
  if (summary) summary.textContent = "Analyzing source file…";
  const requestId = ++pendingFileAnalysisRequest;
  const analysis = await getMediaFileAnalysis(filename).catch(() => null);
  if (requestId !== pendingFileAnalysisRequest) return;
  fileAnalysisCache.set(filename, analysis);
  renderPipeFileAnalysis(filename, analysis);
}

export async function populatePipeFileSelect(selectedFilename = ""): Promise<void> {
  const fileSelect = document.getElementById(
    "pipe-file-input",
  ) as HTMLSelectElement | null;
  if (!fileSelect) return;

  const mediaResult = await listMediaFiles();
  const files = mediaResult?.files ?? [];
  const options = files.map((file: MediaFile) => {
    const labelParts = [file.name];
    if (file.kind === "recording") labelParts.push("recording");
    return `<option value="${escapeHtml(file.name)}">${escapeHtml(labelParts.join(" - "))}</option>`;
  });

  const hasSelectedFile =
    selectedFilename && files.some((file) => file.name === selectedFilename);
  if (selectedFilename && !hasSelectedFile) {
    options.unshift(
      `<option value="${escapeHtml(selectedFilename)}">${escapeHtml(selectedFilename)} (missing)</option>`,
    );
  }

  fileSelect.innerHTML =
    '<option value="">Select file...</option>' + options.join("");
  fileSelect.value = selectedFilename;
}

async function loadPipeFileOptions(selectedFilename = ""): Promise<void> {
  await populatePipeFileSelect(selectedFilename);
  await refreshPipeFileAnalysis(selectedFilename);
}

function resetPipeFileOptions(
  fileIngest: PipelineFileIngestConfig | null,
  fallbackFilename = "",
): void {
  const filename = fileIngest?.configured
    ? fileIngest.filename || ""
    : fallbackFilename;
  const loopCheck = document.getElementById(
    "pipe-file-loop-input",
  ) as HTMLInputElement | null;
  const startInput = document.getElementById(
    "pipe-file-start-time-input",
  ) as HTMLInputElement | null;
  const liveOptimizedInput = document.getElementById(
    "pipe-file-live-optimized-input",
  ) as HTMLInputElement | null;
  const gopInput = document.getElementById(
    "pipe-file-gop-seconds-input",
  ) as HTMLInputElement | null;
  if (loopCheck)
    loopCheck.checked = fileIngest?.configured ? !!fileIngest.loop : false;
  if (startInput)
    startInput.value = fileIngest?.configured
      ? fileIngest.startTime || ""
      : "00:00:00";
  if (liveOptimizedInput) {
    liveOptimizedInput.checked = fileIngest?.configured
      ? !!fileIngest.liveOptimized
      : false;
    liveOptimizedInput.onchange = () => {
      setPipeFileOptimizationUi(liveOptimizedInput.checked);
      void refreshPipeFileAnalysis();
    };
    setPipeFileOptimizationUi(liveOptimizedInput.checked);
  }
  if (gopInput) {
    gopInput.value = String(
      fileIngest?.configured
        ? fileIngest.targetGopSeconds || DEFAULT_FILE_INGEST_GOP_SECONDS
        : DEFAULT_FILE_INGEST_GOP_SECONDS,
    );
    gopInput.oninput = () => {
      const selectedFile =
        (
          document.getElementById("pipe-file-input") as HTMLSelectElement | null
        )?.value?.trim() || filename;
      renderPipeFileAnalysis(
        selectedFile,
        fileAnalysisCache.get(selectedFile) || null,
      );
    };
  }
}

export async function openPipeModal(
  mode: PipeModalMode,
  pipe: PipelineView | null = null,
): Promise<void> {
  currentPipeModalMode = mode;
  currentPipeModalPipeline = pipe;
  (document.getElementById("pipe-mode-input") as HTMLInputElement).value = mode;
  (document.getElementById("pipe-id-input") as HTMLInputElement).value =
    pipe?.id || "";
  (document.getElementById("pipe-name-input") as HTMLInputElement).value =
    pipe?.name || getSuggestedPipelineName();
  const title = document.getElementById("pipe-modal-title");
  if (title)
    title.textContent = mode === "create" ? "Add Pipeline" : "Edit Pipeline";
  const submitBtn = document.getElementById("pipe-submit-btn");
  if (submitBtn)
    submitBtn.textContent = mode === "create" ? "Create" : "Update";

  await populatePipelineKeySelect(pipe?.key ?? "");
  const keySelect = document.getElementById(
    "pipe-stream-key-input",
  ) as HTMLSelectElement | null;
  const keyHint = document.getElementById("pipe-stream-key-locked-hint");
  const keyLocked = pipe ? isPipelineKeyChangeLocked(pipe) : false;
  if (keySelect) keySelect.disabled = keyLocked;
  if (keyHint) keyHint.classList.toggle("hidden", !keyLocked);

  const nameInput = document.getElementById(
    "pipe-name-input",
  ) as HTMLInputElement | null;
  nameInput?.classList.remove("input-error");
  const fileSelect = document.getElementById(
    "pipe-file-input",
  ) as HTMLSelectElement | null;
  fileSelect?.classList.remove("select-error");
  if (fileSelect) {
    fileSelect.onchange = () => {
      void refreshPipeFileAnalysis(fileSelect.value.trim());
    };
  }

  const fallbackFilename = filenameFromInputSource(pipe?.inputSource);
  let fileIngest: PipelineFileIngestConfig | null =
    pipe?.fileIngest !== undefined ? pipe.fileIngest : null;
  if (mode === "edit" && pipe?.id && pipe?.fileIngest === undefined) {
    fileIngest = await getPipelineFileIngest(pipe.id);
  }
  const sourceType =
    fileIngest?.configured || fallbackFilename ? "file" : "publisher";
  setPipeSourceUi(sourceType);
  resetPipeFileOptions(fileIngest, fallbackFilename);
  if (sourceType === "file") {
    await loadPipeFileOptions(fallbackFilename || fileIngest?.filename || "");
  }

  const sourceSelect = document.getElementById(
    "pipe-source-type-input",
  ) as HTMLSelectElement | null;
  if (sourceSelect) {
    sourceSelect.onchange = () => {
      const nextSourceType =
        sourceSelect.value === "file" ? "file" : "publisher";
      setPipeSourceUi(nextSourceType);
      if (nextSourceType === "file") {
        const selectedFilename =
          (
            document.getElementById(
              "pipe-file-input",
            ) as HTMLSelectElement | null
          )?.value?.trim() ||
          filenameFromInputSource(currentPipeModalPipeline?.inputSource) ||
          "";
        void loadPipeFileOptions(selectedFilename);
      }
    };
  }

  populatePipeSrtIngestFields(pipe?.srtIngestPolicy || null);

  (document.getElementById("edit-pipe-modal") as HTMLDialogElement).showModal();
}

function isPipelineKeyChangeLocked(pipe: PipelineView): boolean {
  return !!pipe?.outs?.some((o) => isOutputManagedActive(o));
}

function setPipeSrtIngestModeUi(
  mode: "inherit" | "plaintext" | "encrypted",
): void {
  const passphraseInput = document.getElementById(
    "pipe-srt-ingest-passphrase-input",
  ) as HTMLInputElement | null;
  const pbkeylenInput = document.getElementById(
    "pipe-srt-ingest-pbkeylen-input",
  ) as HTMLSelectElement | null;
  const encrypted = mode === "encrypted";
  if (passphraseInput) {
    passphraseInput.disabled = !encrypted;
    passphraseInput.classList.toggle("input-disabled", !encrypted);
  }
  if (pbkeylenInput) {
    pbkeylenInput.disabled = !encrypted;
    pbkeylenInput.classList.toggle("select-disabled", !encrypted);
  }
}

function populatePipeSrtIngestFields(
  policy?: SrtPipelineIngestConfig | null,
): void {
  const modeInput = document.getElementById(
    "pipe-srt-ingest-mode-input",
  ) as HTMLSelectElement | null;
  const passphraseInput = document.getElementById(
    "pipe-srt-ingest-passphrase-input",
  ) as HTMLInputElement | null;
  const pbkeylenInput = document.getElementById(
    "pipe-srt-ingest-pbkeylen-input",
  ) as HTMLSelectElement | null;
  const mode = policy?.mode || "inherit";
  if (modeInput) {
    modeInput.value = mode;
    modeInput.onchange = () =>
      setPipeSrtIngestModeUi(
        modeInput.value === "encrypted"
          ? "encrypted"
          : modeInput.value === "plaintext"
            ? "plaintext"
            : "inherit",
      );
  }
  if (passphraseInput) passphraseInput.value = policy?.passphrase || "";
  if (pbkeylenInput) pbkeylenInput.value = String(policy?.pbkeylen || 16);
  const details = document.getElementById(
    "pipe-srt-ingest-fields",
  ) as HTMLDetailsElement | null;
  if (details) details.open = mode !== "inherit";
  setPipeSrtIngestModeUi(
    mode === "encrypted"
      ? "encrypted"
      : mode === "plaintext"
        ? "plaintext"
        : "inherit",
  );
}

function readPipeSrtIngestPolicy(): SrtPipelineIngestConfig | null {
  const modeValue =
    (
      document.getElementById(
        "pipe-srt-ingest-mode-input",
      ) as HTMLSelectElement | null
    )?.value || "inherit";
  const mode =
    modeValue === "encrypted"
      ? "encrypted"
      : modeValue === "plaintext"
        ? "plaintext"
        : "inherit";
  const passphrase =
    (
      document.getElementById(
        "pipe-srt-ingest-passphrase-input",
      ) as HTMLInputElement | null
    )?.value.trim() || "";
  const pbkeylenValue = Number(
    (
      document.getElementById(
        "pipe-srt-ingest-pbkeylen-input",
      ) as HTMLSelectElement | null
    )?.value || 16,
  );
  const pbkeylen =
    pbkeylenValue === 24 || pbkeylenValue === 32 ? pbkeylenValue : 16;

  if (
    mode === "encrypted" &&
    (passphrase.length < 10 || passphrase.length > 79)
  ) {
    showErrorAlert("Per-pipeline SRT passphrase must be 10-79 bytes");
    return null;
  }

  return {
    mode,
    passphrase: mode === "encrypted" ? passphrase : null,
    pbkeylen: mode === "encrypted" ? (pbkeylen as 16 | 24 | 32) : null,
  };
}

export async function pipeFormBtn(event: Event): Promise<void> {
  event.preventDefault();

  const modal = document.getElementById(
    "edit-pipe-modal",
  ) as HTMLDialogElement | null;
  const pipeId = (document.getElementById("pipe-id-input") as HTMLInputElement)
    .value;
  const nameInput = document.getElementById(
    "pipe-name-input",
  ) as HTMLInputElement | null;
  const name = nameInput?.value.trim() || "";
  const sourceType =
    (
      document.getElementById(
        "pipe-source-type-input",
      ) as HTMLSelectElement | null
    )?.value === "file"
      ? "file"
      : "publisher";
  const fileSelect = document.getElementById(
    "pipe-file-input",
  ) as HTMLSelectElement | null;
  const filename = fileSelect?.value?.trim() || "";
  const inputSource = sourceType === "file" ? `file:${filename}` : null;

  if (!name) {
    nameInput?.classList.add("input-error");
    return;
  }
  nameInput?.classList.remove("input-error");

  if (sourceType === "file" && !filename) {
    fileSelect?.classList.add("select-error");
    showErrorAlert("Select a file for file ingest");
    return;
  }
  fileSelect?.classList.remove("select-error");

  const srtIngestPolicy = readPipeSrtIngestPolicy();
  if (!srtIngestPolicy) return;

  const streamKey =
    (
      document.getElementById(
        "pipe-stream-key-input",
      ) as HTMLSelectElement | null
    )?.value || "";
  const loopFlag =
    (document.getElementById("pipe-file-loop-input") as HTMLInputElement | null)
      ?.checked ?? false;
  const startTime =
    (
      document.getElementById(
        "pipe-file-start-time-input",
      ) as HTMLInputElement | null
    )?.value.trim() || "";
  const liveOptimized =
    (
      document.getElementById(
        "pipe-file-live-optimized-input",
      ) as HTMLInputElement | null
    )?.checked ?? false;
  const targetGopSeconds = Math.max(
    Number(
      (
        document.getElementById(
          "pipe-file-gop-seconds-input",
        ) as HTMLInputElement | null
      )?.value || DEFAULT_FILE_INGEST_GOP_SECONDS,
    ) || DEFAULT_FILE_INGEST_GOP_SECONDS,
    1,
  );
  const fileIngest =
    sourceType === "file"
      ? {
          filename,
          loopFlag,
          startTime,
          liveOptimized,
          targetGopSeconds,
        }
      : null;

  let savedPipeline: ConfigPipeline | null = currentPipeModalPipeline
    ? {
        id: currentPipeModalPipeline.id,
        name: currentPipeModalPipeline.name,
        streamKey: currentPipeModalPipeline.key || "",
        inputSource: currentPipeModalPipeline.inputSource,
        srtIngestPolicy: currentPipeModalPipeline.srtIngestPolicy || null,
        ingestUrls: currentPipeModalPipeline.ingestUrls,
        fileIngest: currentPipeModalPipeline.fileIngest,
      }
    : null;

  if (currentPipeModalMode === "create") {
    const response = await createPipeline({
      name,
      ...(streamKey ? { streamKey } : {}),
      inputSource,
      srtIngestPolicy,
      fileIngest,
    });
    if (response === null) return;
    savedPipeline = response.pipeline;
  } else {
    const response = await updatePipeline(pipeId, {
      name,
      streamKey,
      inputSource,
      srtIngestPolicy,
      fileIngest,
    });
    if (response === null) return;
    savedPipeline = response.pipeline;
  }

  const savedPipeId = savedPipeline?.id || "";
  if (!savedPipeId || !savedPipeline) return;

  upsertDashboardPipelineConfig(
    savedPipeline,
    savedPipeline.fileIngest || null,
  );
  modal?.close();
  if (currentPipeModalMode === "create") {
    setUrlParam("p", savedPipeId);
  }
}
export async function addPipeBtn(): Promise<void> {
  await openPipeModal("create");
}

export async function editPipeBtn(): Promise<void> {
  const pipeId = getUrlParam("p");
  if (!pipeId) {
    console.error("Please select a pipeline first.");
    return;
  }

  const pipe = state.pipelines.find((p) => p.id === String(pipeId));
  if (!pipe) {
    console.error("Pipeline not found:", pipeId);
    return;
  }

  await openPipeModal("edit", pipe);
}

export async function deletePipeBtn(): Promise<void> {
  const pipeId = getUrlParam("p");
  if (!pipeId) {
    console.error("Please select a pipeline first.");
    return;
  }

  const pipe = state.pipelines.find((p) => p.id === pipeId);
  if (!pipe) {
    console.error("Pipeline not found:", pipeId);
    return;
  }

  if (
    !(await confirmInApp({
      title: "Delete Pipeline",
      message: `Delete pipeline "${pipe.name}"?`,
      confirmLabel: "Delete",
      destructive: true,
    }))
  ) {
    return;
  }

  const res = await deletePipeline(pipeId);
  if (res === null) return;

  setUrlParam("p", null);
  removeDashboardPipelineConfig(pipeId);
}
