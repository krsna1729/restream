import {
  deleteMediaFile,
  listMediaFiles,
  renameMediaFile,
  uploadMediaFile,
  type MediaFile,
} from "../core/api.js";
import { withBasePath } from "../core/base-path.js";
import {
  confirmInApp,
  escapeHtml,
  promptInApp,
  showErrorAlert,
} from "../core/utils.js";
import { state } from "../core/state.js";
import type { MediaCheckpointModel } from "./media-view-model.js";

type MediaKind = "recording" | "source";
let mediaRefreshInFlight: Promise<void> | null = null;
let lastMediaSignature = "";
let lastRecordingsSignature = "";
let lastSourcesSignature = "";
let mediaShellMounted = false;
let nativePlaybackProbe: HTMLVideoElement | null | undefined;
let mediaSearchQuery = "";
let lastMediaFiles: MediaFile[] = [];
let mediaRecordingsExpanded = false;
let mediaSourcesExpanded = false;
const mediaActionRowsExpanded = new Set<string>();
let mediaCheckpointCallback:
  | ((model: MediaCheckpointModel | null) => void)
  | null = null;

const MEDIA_SECTION_VISIBLE_LIMIT = 8;

function formatFileSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  if (bytes < 1024 * 1024 * 1024)
    return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

function formatModified(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "--";
  return date.toLocaleString();
}

function mediaKind(file: MediaFile): MediaKind {
  if (file.kind === "recording" || file.kind === "source") {
    return file.kind;
  }
  const name = file.name.toLowerCase();
  if ((file.ingestCount ?? 0) > 0) return "source";
  if (name.includes("recording")) return "recording";
  return "source";
}

function sectionEmpty(label: string): string {
  return `<div class="dashboard-empty">No ${escapeHtml(label)}.</div>`;
}

function filteredSectionEmptyLabel(kind: "recordings" | "source files"): string {
  const query = mediaSearchQuery.trim();
  return `${kind} match "${query}". Clear search to return to the full recording/source split`;
}

function fileCountLabel(count: number, singular: string): string {
  return `${count} ${singular}${count === 1 ? "" : "s"}`;
}

function normalizeSearchText(value: string | null | undefined): string {
  return String(value || "").trim().toLowerCase();
}

export function mediaFileMatchesSearch(
  file: MediaFile,
  query: string,
): boolean {
  const normalizedQuery = normalizeSearchText(query);
  if (!normalizedQuery) return true;
  const haystack = [
    file.name,
    file.sourceName,
    file.convertedName,
    file.playName,
    file.kind,
    file.conversionStatus,
  ]
    .map((value) => normalizeSearchText(value))
    .join(" ");
  return haystack.includes(normalizedQuery);
}

function mediaContentTypeForName(
  name: string | null | undefined,
): string | null {
  const extension = name?.split(".").pop()?.toLowerCase() ?? "";
  switch (extension) {
    case "mp4":
      return "video/mp4";
    case "mov":
      return "video/quicktime";
    case "mkv":
      return "video/x-matroska";
    case "ts":
      return "video/mp2t";
    default:
      return null;
  }
}

function getNativePlaybackProbe(): HTMLVideoElement | null {
  if (nativePlaybackProbe !== undefined) return nativePlaybackProbe;
  const probe = document.createElement("video");
  nativePlaybackProbe = typeof probe.canPlayType === "function" ? probe : null;
  return nativePlaybackProbe;
}

function isNativelyPlayable(file: MediaFile): boolean {
  const contentType = mediaContentTypeForName(file.playName ?? file.name);
  const probe = getNativePlaybackProbe();
  if (!contentType || !probe) return false;
  return probe.canPlayType(contentType).trim() !== "";
}

function mediaV2Active(): boolean {
  const toggle = document.getElementById("dashboard-ui-v2-toggle");
  if (toggle instanceof HTMLInputElement && toggle.checked) return true;
  try {
    return new URLSearchParams(window.location.search).get("ui") === "v2";
  } catch (_err) {
    return false;
  }
}

function mediaRowSecondaryActions(
  file: MediaFile,
  safeName: string,
  deleteDisabled: string,
  downloadActions: string,
): string {
  const buttons = `<button class="btn btn-xs btn-outline shrink-0 js-rename-media" data-filename="${safeName}" aria-label="Rename ${safeName}">Rename</button>
        <button class="btn btn-xs btn-error btn-outline shrink-0 js-delete-media" data-filename="${safeName}" aria-label="Delete ${safeName}" ${deleteDisabled}>Delete</button>`;
  if (!mediaV2Active()) return `${downloadActions}${buttons}`;
  const expanded = mediaActionRowsExpanded.has(file.name);
  return `<div class="flex shrink-0 flex-wrap items-center justify-end gap-2">
        <button class="btn btn-xs btn-outline js-media-row-actions" type="button" data-filename="${safeName}" aria-expanded="${expanded ? "true" : "false"}" aria-label="${expanded ? "Hide" : "Show"} actions for ${safeName}">${expanded ? "Hide actions" : "More actions"}</button>
        ${expanded ? `${downloadActions}${buttons}` : ""}
    </div>`;
}

function mediaFileRow(file: MediaFile): string {
  const safeName = escapeHtml(file.name);
  const sourceName = file.sourceName || file.name;
  const sourceUrl = withBasePath(`/media/${encodeURIComponent(sourceName)}`);
  const playName = file.playName || null;
  const playUrl = playName
    ? withBasePath(`/media/${encodeURIComponent(playName)}`)
    : null;
  const convertedName = file.convertedName || null;
  const convertedUrl =
    convertedName && convertedName !== sourceName
      ? withBasePath(`/media/${encodeURIComponent(convertedName)}`)
      : null;
  const hasIngests = (file.ingestCount ?? 0) > 0;
  const deleteDisabled = hasIngests
    ? 'disabled title="Remove configured ingests first"'
    : "";
  const canPlay = isNativelyPlayable(file);
  const playAction =
    canPlay && playUrl
      ? `<a href="${playUrl}" target="_blank" rel="noopener noreferrer" class="btn btn-xs btn-accent btn-outline shrink-0" aria-label="Play ${safeName}">Play</a>`
      : `<button type="button" class="btn btn-xs btn-accent btn-outline shrink-0" disabled aria-label="Play ${safeName} unavailable" title="This file is not ready for native Chrome playback yet">Play</button>`;
  const conversionStatusBadge =
    file.conversionStatus === "converting"
      ? '<span class="badge badge-sm badge-warning">Converting</span>'
      : file.conversionStatus === "ready"
        ? '<span class="badge badge-sm badge-success">MP4 Ready</span>'
        : file.conversionStatus === "failed"
          ? `<span class="badge badge-sm badge-error" title="${escapeHtml(file.conversionError || "Conversion failed")}">Conversion Failed</span>`
          : "";
  const downloadActions = convertedUrl
    ? `<a href="${convertedUrl}" download="${escapeHtml(convertedName || "")}" class="btn btn-xs btn-accent btn-outline shrink-0" aria-label="Download MP4 for ${safeName}">Download MP4</a>
        <a href="${sourceUrl}" download="${escapeHtml(sourceName)}" class="btn btn-xs btn-outline shrink-0" aria-label="Download TS for ${safeName}">Download TS</a>`
    : `<a href="${sourceUrl}" download="${escapeHtml(sourceName)}" class="btn btn-xs btn-accent btn-outline shrink-0" aria-label="Download ${safeName}">Download</a>`;
  const sizeLabel =
    convertedUrl && file.convertedSize
      ? `${formatFileSize(file.sourceSize ?? file.size)} TS / ${formatFileSize(file.convertedSize)} MP4`
      : formatFileSize(file.sourceSize ?? file.size);

  return `<div class="border-base-content/10 bg-base-100 flex min-h-18 flex-wrap items-center gap-3 rounded-lg border px-3 py-2" data-filename="${safeName}">
        <div class="min-w-0 flex-1">
            <div class="flex min-w-0 flex-wrap items-center gap-2">
                <div class="truncate text-sm font-semibold" title="${safeName}">${safeName}</div>
                ${conversionStatusBadge}
            </div>
            <div class="text-base-content/55 mt-1 flex flex-wrap gap-x-3 gap-y-1 text-xs">
                <span>${escapeHtml(sizeLabel)}</span>
                <span>${escapeHtml(formatModified(file.modifiedAt))}</span>
                ${hasIngests ? `<span>${file.ingestCount} ingest${file.ingestCount === 1 ? "" : "s"}</span>` : ""}
            </div>
        </div>
        ${playAction}
        ${mediaRowSecondaryActions(file, safeName, deleteDisabled, downloadActions)}
    </div>`;
}

function mediaSectionShell(
  title: string,
  listId: string,
  summaryId: string,
  toggleId: string,
): string {
  return `<section class="dashboard-section">
        <div class="dashboard-section-header">
            <h2 class="dashboard-section-title">${escapeHtml(title)}</h2>
            <div class="flex flex-wrap items-center justify-end gap-2">
                <span class="dashboard-muted" id="${summaryId}">--</span>
                <button id="${toggleId}" type="button" class="btn btn-xs btn-outline hidden" aria-expanded="false">Show all</button>
            </div>
        </div>
        <div class="space-y-2 p-3" id="${listId}"></div>
    </section>`;
}

function mediaDiskSummaryHtml(): string {
  const disk = state.metrics.mediaDisk;
  if (!disk) return "";
  const used = formatFileSize(disk.usedBytes ?? 0);
  const total = formatFileSize(disk.totalBytes ?? 0);
  const percent = Number.isFinite(disk.usedPercent as number)
    ? `${(disk.usedPercent as number).toFixed(0)}%`
    : "--";
  return `<section class="dashboard-stat-card">
        <div class="dashboard-kicker">Media Disk</div>
        <div class="mt-2 text-2xl font-semibold tabular-nums">${escapeHtml(percent)}</div>
        <div class="dashboard-muted mt-1">${escapeHtml(used)} / ${escapeHtml(total)}</div>
    </section>`;
}

function mediaDiskLabel(): string {
  const disk = state.metrics.mediaDisk;
  if (!disk) return "Storage --";
  const used = formatFileSize(disk.usedBytes ?? 0);
  const total = formatFileSize(disk.totalBytes ?? 0);
  const percent = Number.isFinite(disk.usedPercent as number)
    ? `${(disk.usedPercent as number).toFixed(0)}%`
    : "--";
  return `${percent} · ${used} / ${total}`;
}

function buildMediaCheckpointModel(files: MediaFile[]): MediaCheckpointModel {
  const recordings = files.filter((file) => mediaKind(file) === "recording");
  const sources = files.filter((file) => mediaKind(file) !== "recording");
  const filteredRecordings = recordings.filter((file) =>
    mediaFileMatchesSearch(file, mediaSearchQuery),
  );
  const filteredSources = sources.filter((file) =>
    mediaFileMatchesSearch(file, mediaSearchQuery),
  );
  const filteredTotal = filteredRecordings.length + filteredSources.length;
  const totalBytes = files.reduce((sum, file) => sum + file.size, 0);
  const query = mediaSearchQuery.trim();
  const searchLabel = query
    ? `${filteredTotal}/${files.length} visible`
    : `${files.length} visible`;
  return {
    canOpenOverview: true,
    focusLabel: query
      ? `${filteredTotal} media file${filteredTotal === 1 ? "" : "s"} match "${query}". Clear search to return to the full recording/source split.`
      : files.length
        ? "Use search when the library grows; dense recording and source lists stay bounded until you ask for more."
        : "The media directory is empty. Upload a source file or enable recording from a pipeline.",
    metrics: [
      { label: "Total size", value: formatFileSize(totalBytes) },
      { label: "Disk", value: mediaDiskLabel() },
    ],
    nextStep: files.length
      ? "Open a source, download a recording, or jump back to Overview to wire media into live pipelines."
      : "Upload media or start a pipeline recording.",
    recordingLabel: fileCountLabel(recordings.length, "recording"),
    searchLabel,
    sourceLabel: fileCountLabel(sources.length, "source file"),
    statusLabel: query ? "Filtered" : files.length ? "Loaded" : "Empty",
    statusTone: query ? "warning" : files.length ? "success" : "neutral",
    storageLabel: mediaDiskLabel(),
    summary: query
      ? `${filteredTotal}/${files.length} media file${filteredTotal === 1 ? "" : "s"} shown · ${fileCountLabel(filteredRecordings.length, "recording")} · ${fileCountLabel(filteredSources.length, "source file")} matched · "${query}"`
      : `${fileCountLabel(files.length, "media file")} total · ${fileCountLabel(recordings.length, "recording")} · ${fileCountLabel(sources.length, "source file")}`,
    title: "Media",
  };
}

function publishMediaCheckpoint(files: MediaFile[]): void {
  mediaCheckpointCallback?.(buildMediaCheckpointModel(files));
}

export function configureMediaCheckpointPresentation(options: {
  onPresentation?: (model: MediaCheckpointModel | null) => void;
}): void {
  mediaCheckpointCallback = options.onPresentation || null;
  if (mediaCheckpointCallback) {
    mediaCheckpointCallback(buildMediaCheckpointModel(lastMediaFiles));
  }
}

function mountMediaShell(container: HTMLElement): void {
  if (mediaShellMounted && document.getElementById("media-library-root"))
    return;
  container.innerHTML = `<div class="space-y-4" id="media-library-root">
        <div class="grid gap-3 md:grid-cols-3">
            <section class="dashboard-stat-card">
                <div class="dashboard-kicker">Recordings</div>
                <div class="mt-2 text-2xl font-semibold tabular-nums" id="media-recording-count">--</div>
                <div class="dashboard-muted mt-1" id="media-recording-size">--</div>
            </section>
            <section class="dashboard-stat-card">
                <div class="dashboard-kicker">Source Files</div>
                <div class="mt-2 text-2xl font-semibold tabular-nums" id="media-source-count">--</div>
                <div class="dashboard-muted mt-1" id="media-source-size">--</div>
            </section>
            <div id="media-disk-summary">${mediaDiskSummaryHtml()}</div>
        </div>
        <section class="dashboard-section">
            <div class="dashboard-section-header">
                <div>
                    <h1 class="dashboard-title">Media Library</h1>
                    <p class="dashboard-subtitle">Recordings and file-ingest sources from the configured media directory.</p>
                </div>
                <div class="flex min-w-0 flex-wrap items-center justify-end gap-2">
                    <label class="input input-sm input-bordered flex min-w-56 max-w-80 flex-1 items-center gap-2">
                        <span class="text-base-content/50 text-xs font-semibold uppercase">Search</span>
                        <input id="media-library-search" class="grow" type="search" autocomplete="off" placeholder="filename, kind, status" aria-label="Search media library" value="${escapeHtml(mediaSearchQuery)}">
                    </label>
                    <button id="media-library-clear-search-btn" type="button" class="btn btn-sm btn-outline hidden" aria-label="Clear media library search">Clear search</button>
                    <input class="hidden js-upload-media-input" type="file" accept=".ts,.mkv,.mp4,.mov">
                    <button type="button" class="btn btn-sm btn-primary js-upload-media" aria-label="Upload media file">Upload media</button>
                </div>
            </div>
            <p id="media-library-results-summary" class="dashboard-muted px-4 pt-3 text-xs" role="status" aria-live="polite">--</p>
            <div class="space-y-4 p-4">
                ${mediaSectionShell("Recordings", "media-recordings-list", "media-recordings-summary", "media-recordings-toggle")}
                ${mediaSectionShell("Source Files", "media-sources-list", "media-sources-summary", "media-sources-toggle")}
            </div>
        </section>
    </div>`;
  mediaShellMounted = true;
}

function fileListSignature(files: MediaFile[]): string {
  return JSON.stringify(
    files.map((file) => [
      file.name,
      file.size,
      file.modifiedAt,
      file.ingestCount ?? 0,
      mediaKind(file),
      file.sourceName ?? "",
      file.convertedName ?? "",
      file.playName ?? "",
      file.conversionStatus ?? "",
      file.conversionError ?? "",
    ]),
  );
}

function setText(id: string, value: string | number): void {
  const el = document.getElementById(id);
  if (el && el.textContent !== String(value)) el.textContent = String(value);
}

function setHtmlIfChanged(id: string, html: string): boolean {
  const el = document.getElementById(id);
  if (!el || el.innerHTML === html) return false;
  el.innerHTML = html;
  return true;
}

export function refreshMediaLibraryMetricsOnly(): void {
  if (!mediaShellMounted || !document.getElementById("media-library-root"))
    return;
  setHtmlIfChanged("media-disk-summary", mediaDiskSummaryHtml());
  publishMediaCheckpoint(lastMediaFiles);
}

export function resetMediaLibraryShellState(): void {
  mediaShellMounted = false;
  lastMediaSignature = "";
  lastRecordingsSignature = "";
  lastSourcesSignature = "";
  mediaActionRowsExpanded.clear();
}

function attachMediaActions(container: HTMLElement): void {
  const searchInput = container.querySelector<HTMLInputElement>(
    "#media-library-search",
  );
  if (searchInput && searchInput.dataset.bound !== "1") {
    searchInput.dataset.bound = "1";
    searchInput.addEventListener("input", () => {
      mediaSearchQuery = searchInput.value;
      mediaRecordingsExpanded = false;
      mediaSourcesExpanded = false;
      mediaActionRowsExpanded.clear();
      renderMediaLibraryLists(lastMediaFiles, true);
    });
  }
  const clearSearchButton = container.querySelector<HTMLButtonElement>(
    "#media-library-clear-search-btn",
  );
  if (clearSearchButton && clearSearchButton.dataset.bound !== "1") {
    clearSearchButton.dataset.bound = "1";
    clearSearchButton.addEventListener("click", () => {
      mediaSearchQuery = "";
      mediaRecordingsExpanded = false;
      mediaSourcesExpanded = false;
      mediaActionRowsExpanded.clear();
      renderMediaLibraryLists(lastMediaFiles, true);
      const nextSearchInput = container.querySelector<HTMLInputElement>(
        "#media-library-search",
      );
      if (nextSearchInput) {
        nextSearchInput.value = "";
        nextSearchInput.focus();
      }
    });
  }
  const recordingsToggle = container.querySelector<HTMLButtonElement>(
    "#media-recordings-toggle",
  );
  if (recordingsToggle && recordingsToggle.dataset.bound !== "1") {
    recordingsToggle.dataset.bound = "1";
    recordingsToggle.addEventListener("click", () => {
      mediaRecordingsExpanded = !mediaRecordingsExpanded;
      renderMediaLibraryLists(lastMediaFiles, true);
    });
  }
  const sourcesToggle = container.querySelector<HTMLButtonElement>(
    "#media-sources-toggle",
  );
  if (sourcesToggle && sourcesToggle.dataset.bound !== "1") {
    sourcesToggle.dataset.bound = "1";
    sourcesToggle.addEventListener("click", () => {
      mediaSourcesExpanded = !mediaSourcesExpanded;
      renderMediaLibraryLists(lastMediaFiles, true);
    });
  }
  container
    .querySelectorAll<HTMLButtonElement>(".js-media-row-actions")
    .forEach((btn) => {
      if (btn.dataset.bound === "1") return;
      btn.dataset.bound = "1";
      btn.addEventListener("click", () => {
        const filename = btn.dataset.filename;
        if (!filename) return;
        if (mediaActionRowsExpanded.has(filename)) {
          mediaActionRowsExpanded.delete(filename);
        } else {
          mediaActionRowsExpanded.add(filename);
        }
        renderMediaLibraryLists(lastMediaFiles, true);
      });
    });

  const uploadButton = container.querySelector<HTMLButtonElement>(".js-upload-media");
  const uploadInput = container.querySelector<HTMLInputElement>(".js-upload-media-input");
  if (uploadButton && uploadInput && uploadButton.dataset.bound !== "1") {
    uploadButton.dataset.bound = "1";
    uploadButton.addEventListener("click", () => uploadInput.click());
    uploadInput.addEventListener("change", async () => {
      const file = uploadInput.files?.[0];
      uploadInput.value = "";
      if (!file) return;
      const result = await uploadMediaFile(file);
      if (result !== null) await renderMediaLibraryMode({ force: true });
    });
  }
  container
    .querySelectorAll<HTMLButtonElement>(".js-rename-media")
    .forEach((btn) => {
      if (btn.dataset.bound === "1") return;
      btn.dataset.bound = "1";
      btn.addEventListener("click", async () => {
        const filename = btn.dataset.filename;
        if (!filename) return;
        const nextName = await promptInApp({
          title: "Rename Media File",
          message:
            "Choose a new filename. The file extension must stay the same.",
          initialValue: filename,
          confirmLabel: "Rename",
          placeholder: filename,
        });
        if (nextName === null) return;
        const trimmed = nextName.trim();
        if (!trimmed || trimmed === filename) return;
        const res = await renameMediaFile(filename, trimmed);
        if (res === null) {
          showErrorAlert("Rename failed");
          return;
        }
        await renderMediaLibraryMode({ force: true });
      });
    });
  container
    .querySelectorAll<HTMLButtonElement>(".js-delete-media")
    .forEach((btn) => {
      if (btn.dataset.bound === "1") return;
      btn.dataset.bound = "1";
      btn.addEventListener("click", async () => {
        const filename = btn.dataset.filename;
        if (!filename) return;
        const confirmed = await confirmInApp({
          title: "Delete Media File",
          message: `Permanently delete "${filename}"?`,
          confirmLabel: "Delete",
          destructive: true,
        });
        if (!confirmed) return;
        const res = await deleteMediaFile(filename);
        if (res !== null) await renderMediaLibraryMode({ force: true });
      });
    });
}

function updateSection(
  listId: string,
  summaryId: string,
  toggleId: string,
  files: MediaFile[],
  totalFiles: MediaFile[],
  emptyLabel: string,
  previousSignature: string,
  expanded: boolean,
  sectionLabel: string,
): string {
  const isFiltered = normalizeSearchText(mediaSearchQuery) !== "";
  const showToggle = !isFiltered && files.length > MEDIA_SECTION_VISIBLE_LIMIT;
  const visibleFiles =
    showToggle && !expanded ? files.slice(0, MEDIA_SECTION_VISIBLE_LIMIT) : files;
  const signature = JSON.stringify({
    files: fileListSignature(visibleFiles),
    query: normalizeSearchText(mediaSearchQuery),
    totalCount: files.length,
    expanded,
    actionRows: visibleFiles
      .filter((file) => mediaActionRowsExpanded.has(file.name))
      .map((file) => file.name),
  });
  const totalBytes = files.reduce((sum, file) => sum + file.size, 0);
  const totalCount = totalFiles.length;
  setText(
    summaryId,
    showToggle
      ? `${visibleFiles.length} shown of ${files.length} files / ${formatFileSize(totalBytes)}`
      : isFiltered
        ? `${files.length} of ${totalCount} file${totalCount === 1 ? "" : "s"} / ${formatFileSize(totalBytes)}`
        : `${files.length} file${files.length === 1 ? "" : "s"} / ${formatFileSize(totalBytes)}`,
  );
  const toggle = document.getElementById(toggleId) as HTMLButtonElement | null;
  if (toggle) {
    toggle.classList.toggle("hidden", !showToggle);
    toggle.setAttribute("aria-expanded", expanded ? "true" : "false");
    toggle.textContent = expanded ? "Show fewer" : `Show all ${files.length}`;
    toggle.setAttribute(
      "aria-label",
      expanded
        ? `Show fewer ${sectionLabel}`
        : `Show all ${files.length} ${sectionLabel}`,
    );
  }
  if (signature !== previousSignature) {
    setHtmlIfChanged(
      listId,
      visibleFiles.length
        ? visibleFiles.map(mediaFileRow).join("")
        : sectionEmpty(emptyLabel),
    );
  }
  return signature;
}

function renderMediaLibraryLists(files: MediaFile[], force: boolean): void {
  const knownFiles = new Set(files.map((file) => file.name));
  for (const filename of mediaActionRowsExpanded) {
    if (!knownFiles.has(filename)) mediaActionRowsExpanded.delete(filename);
  }
  const recordings = files.filter((file) => mediaKind(file) === "recording");
  const sources = files.filter((file) => mediaKind(file) !== "recording");
  const filteredRecordings = recordings.filter((file) =>
    mediaFileMatchesSearch(file, mediaSearchQuery),
  );
  const filteredSources = sources.filter((file) =>
    mediaFileMatchesSearch(file, mediaSearchQuery),
  );
  const totalBytes = files.reduce((sum, file) => sum + file.size, 0);
  const recordingBytes = recordings.reduce((sum, file) => sum + file.size, 0);
  const filteredTotal = filteredRecordings.length + filteredSources.length;
  const query = mediaSearchQuery.trim();
  document
    .getElementById("media-library-clear-search-btn")
    ?.classList.toggle("hidden", normalizeSearchText(mediaSearchQuery) === "");
  setText("media-recording-count", recordings.length);
  setText("media-recording-size", formatFileSize(recordingBytes));
  setText("media-source-count", sources.length);
  setText("media-source-size", formatFileSize(totalBytes - recordingBytes));
  const sectionSplit = `${fileCountLabel(filteredRecordings.length, "recording")} · ${fileCountLabel(filteredSources.length, "source file")}`;
  setText(
    "media-library-results-summary",
    query
      ? `${filteredTotal}/${files.length} media file${filteredTotal === 1 ? "" : "s"} shown · ${sectionSplit} matched · "${query}"`
      : `${fileCountLabel(files.length, "media file")} total · ${sectionSplit}`,
  );
  const isFiltered = normalizeSearchText(mediaSearchQuery) !== "";
  lastRecordingsSignature = updateSection(
    "media-recordings-list",
    "media-recordings-summary",
    "media-recordings-toggle",
    filteredRecordings,
    recordings,
    isFiltered ? filteredSectionEmptyLabel("recordings") : "recordings yet",
    force ? "" : lastRecordingsSignature,
    mediaRecordingsExpanded,
    "recordings",
  );
  lastSourcesSignature = updateSection(
    "media-sources-list",
    "media-sources-summary",
    "media-sources-toggle",
    filteredSources,
    sources,
    isFiltered ? filteredSectionEmptyLabel("source files") : "source files",
    force ? "" : lastSourcesSignature,
    mediaSourcesExpanded,
    "source files",
  );
  const root = document.getElementById("media-library-root");
  if (root) attachMediaActions(root);
  publishMediaCheckpoint(files);
}

export async function renderMediaLibraryMode({
  force = false,
}: { force?: boolean } = {}): Promise<void> {
  const container = document.getElementById("media-mode-content");
  if (!container) return;
  if (mediaRefreshInFlight && !force) return mediaRefreshInFlight;

  mountMediaShell(container);

  mediaRefreshInFlight = (async () => {
    const result = await listMediaFiles();
    const files = [...(result?.files ?? [])].sort((a, b) => {
      const aTime = new Date(a.modifiedAt).getTime() || 0;
      const bTime = new Date(b.modifiedAt).getTime() || 0;
      return bTime - aTime || a.name.localeCompare(b.name);
    });
    const diskHtml = mediaDiskSummaryHtml();
    const signature = JSON.stringify({
      files: fileListSignature(files),
      mediaDisk: diskHtml,
      search: normalizeSearchText(mediaSearchQuery),
    });
    if (!force && signature === lastMediaSignature) return;

    lastMediaFiles = files;
    setHtmlIfChanged("media-disk-summary", diskHtml);
    renderMediaLibraryLists(files, force);
    attachMediaActions(container);
    lastMediaSignature = signature;
  })();

  try {
    await mediaRefreshInFlight;
  } finally {
    mediaRefreshInFlight = null;
  }
}
