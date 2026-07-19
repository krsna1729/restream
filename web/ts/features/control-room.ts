import {
  copyText,
  escapeHtml,
  isValidMonitoringUrl,
  showCopiedNotification,
  showErrorAlert,
} from "../core/utils.js";
import { getYoutubeMonitoringStatus, updateOutput } from "../core/api.js";
import type { YoutubeMonitoringStatus } from "../core/api.js";
import { state } from "../core/state.js";
import {
  clearManagedHlsPlayer,
  getManagedHlsController,
  renderManagedHlsPlayer,
} from "./hls-player.js";
import { buildInputPreviewUrl } from "./input-preview.js";
import { upsertDashboardOutputConfig } from "./dashboard.js";
import type { OutputView, PipelineInput, PipelineView } from "../types.js";
import { normalizeOutputConfig } from "../core/output-config.js";
import type { ControlRoomCheckpointModel } from "./control-room-view-model.js";
import {
  buildControlRoomCheckpointModel,
  controlRoomScopeSummaryText,
} from "./control-room-checkpoint.js";
import type {
  ControlRoomCardDescriptor,
  ControlRoomMediaController,
  ControlRoomOutputOption,
  ControlRoomState,
  ControlRoomWorkspaceDependencies,
  MonitoringEmbedKind,
  YouTubeApiNamespace,
  YouTubePlayerApi,
} from "./control-room-types.js";
import {
  buildControlRoomInputCard,
  controlRoomInputs,
  isControlRoomInputPromotionPending,
  promoteControlRoomInput,
} from "./control-room-inputs.js";

declare global {
  interface Window {
    YT?: YouTubeApiNamespace;
    onYouTubeIframeAPIReady?: (() => void) | undefined;
  }
}

const CONTROL_ROOM_STATE_KEY = "dashboard:control-room-state";
const OUTPUTS_PER_PAGE = 11;
const CONTROL_ROOM_PLAYER_HEIGHT_CLASS = "h-[11rem]";
const CONTROL_ROOM_MONITOR_FRAME_CLASS =
  "relative isolate w-full overflow-hidden rounded-[0.9rem] bg-neutral-950";
const CONTROL_ROOM_MONITOR_BUTTON_CLASS =
  "btn btn-xs border border-white/15 bg-black/55 text-white shadow-sm backdrop-blur hover:border-white/25 hover:bg-black/75";
const CONTROL_ROOM_CARD_BASE_CLASS =
  "group flex min-h-[17rem] min-w-0 w-full max-w-full flex-col overflow-hidden rounded-2xl border p-3 shadow-[0_18px_45px_rgba(15,23,42,0.12)]";

let controlRoomStateLoaded = false;
let workspaceSelectionOwned = false;
const workspaceDependencies: ControlRoomWorkspaceDependencies = {
  selectedPipelineId: () => null,
  selectPipeline: () => {},
  openMonitorView: () => window.setDashboardMode?.("control"),
};
let controlRoomState: ControlRoomState = {
  pipelineId: null,
  page: 0,
  searchQuery: "",
};
const controlRoomMonitoringDrafts = new Map<string, string>();
const controlRoomMonitoringSavePending = new Set<string>();
const controlRoomCardWarnings = new Map<string, string>();
const controlRoomCardActionsExpanded = new Set<string>();
const youtubeMonitoringStatusCache = new Map<
  string,
  {
    expiresAt: number;
    data: YoutubeMonitoringStatus | null;
    pending?: Promise<YoutubeMonitoringStatus | null>;
  }
>();
const controlRoomMediaControllers = new WeakMap<
  HTMLElement,
  ControlRoomMediaController
>();
const controlRoomLoadedEmbedCards = new Set<string>();
let pendingMonitoringInputFocusOutputId: string | null = null;
let youtubeIframeApiPromise: Promise<YouTubeApiNamespace> | null = null;
let controlRoomPlaybackIntent: "play" | "pause" = "play";
let controlRoomMuteIntent: "mute" | "unmute" = "mute";
let controlRoomCheckpointCallback:
  ((model: ControlRoomCheckpointModel | null) => void) | null = null;
const controlRoomNameCollator = new Intl.Collator(undefined, {
  numeric: true,
  sensitivity: "base",
});
const YOUTUBE_MONITORING_STATUS_TTL_MS = 60_000;

function controlRoomV2Active(): boolean {
  const toggle = document.getElementById("dashboard-ui-v2-toggle");
  if (toggle instanceof HTMLInputElement && toggle.checked) return true;
  try {
    return new URLSearchParams(window.location.search).get("ui") === "v2";
  } catch (_err) {
    return false;
  }
}

export function configureControlRoomCheckpointPresentation(options: {
  readonly onPresentation?: (model: ControlRoomCheckpointModel | null) => void;
}): void {
  controlRoomCheckpointCallback = options.onPresentation ?? null;
}

function listPipelines(): PipelineView[] {
  return [...state.pipelines].sort((a, b) =>
    controlRoomNameCollator.compare(a.name, b.name),
  );
}

function listMonitoringOutputsForPipeline(
  pipelineId: string,
): ControlRoomOutputOption[] {
  const pipe = state.pipelines.find((candidate) => candidate.id === pipelineId);
  if (!pipe) return [];
  return pipe.outs
    .filter((out) => !!out.monitoringUrl)
    .map((out) => ({
      outputId: out.id,
      pipelineId: pipe.id,
      pipelineName: pipe.name,
      outputName: out.name,
      monitoringUrl: out.monitoringUrl,
      status: out.status,
      flapping: out.flapping,
    }))
    .sort((a, b) =>
      controlRoomNameCollator.compare(a.outputName, b.outputName),
    );
}

function isPreviewableOutputStatus(status: string | null | undefined): boolean {
  const normalized = (status || "").trim().toLowerCase();
  return (
    normalized === "on" || normalized === "running" || normalized === "warning"
  );
}

function getDefaultPipelineId(): string | null {
  const pipelines = listPipelines();
  const withMonitoring = pipelines.find((pipe) =>
    pipe.outs.some((out) => !!out.monitoringUrl),
  );
  return withMonitoring?.id || pipelines[0]?.id || null;
}

function normalizeState(): void {
  const pipelines = listPipelines();
  if (pipelines.length === 0) {
    controlRoomState.pipelineId = null;
    controlRoomState.page = 0;
    return;
  }

  if (
    !controlRoomState.pipelineId ||
    !pipelines.some((pipe) => pipe.id === controlRoomState.pipelineId)
  ) {
    controlRoomState.pipelineId = workspaceSelectionOwned
      ? null
      : getDefaultPipelineId();
  }

  const selectedPipelineId = controlRoomState.pipelineId;
  if (!selectedPipelineId) {
    controlRoomState.page = 0;
    return;
  }

  const outputs = filterMonitoringOutputs(
    listMonitoringOutputsForPipeline(selectedPipelineId),
    controlRoomState.searchQuery,
  );
  const pageCount = Math.max(1, Math.ceil(outputs.length / OUTPUTS_PER_PAGE));
  controlRoomState.page = Math.min(
    Math.max(0, controlRoomState.page),
    pageCount - 1,
  );
}

export function setControlRoomWorkspaceDependencies(
  dependencies: Partial<ControlRoomWorkspaceDependencies>,
): void {
  Object.assign(workspaceDependencies, dependencies || {});
  workspaceSelectionOwned = true;
}

export function syncControlRoomWorkspaceSelection(): string | null {
  if (workspaceSelectionOwned) {
    controlRoomState.pipelineId = workspaceDependencies.selectedPipelineId();
  }
  normalizeState();
  return controlRoomState.pipelineId;
}

export function selectControlRoomPipeline(pipelineId: string | null): void {
  controlRoomState.pipelineId = pipelineId;
  controlRoomState.page = 0;
  normalizeState();
  persistState();
  workspaceDependencies.selectPipeline(controlRoomState.pipelineId);
}

function persistState(): void {
  try {
    window.localStorage.setItem(
      CONTROL_ROOM_STATE_KEY,
      JSON.stringify(controlRoomState),
    );
  } catch {
    // Ignore storage failures so the control room stays usable.
  }
}

function ensureStateLoaded(): void {
  if (controlRoomStateLoaded) return;
  controlRoomStateLoaded = true;
  try {
    const raw = window.localStorage.getItem(CONTROL_ROOM_STATE_KEY);
    if (!raw) {
      normalizeState();
      return;
    }
    const parsed = JSON.parse(raw);
    controlRoomState = {
      pipelineId:
        typeof parsed?.pipelineId === "string" && parsed.pipelineId.trim()
          ? parsed.pipelineId
          : null,
      page: Number.isFinite(parsed?.page)
        ? Math.max(0, Number(parsed.page))
        : 0,
      searchQuery:
        typeof parsed?.searchQuery === "string" ? parsed.searchQuery : "",
    };
  } catch {
    controlRoomState = { pipelineId: null, page: 0, searchQuery: "" };
  }
  normalizeState();
}

function listMountedMediaControllers(
  scope: ParentNode = document,
): Array<{ shell: HTMLElement; controller: ControlRoomMediaController }> {
  const result: Array<{
    shell: HTMLElement;
    controller: ControlRoomMediaController;
  }> = [];
  const shells = scope.querySelectorAll<HTMLElement>(
    '[data-role="control-room-player-shell"]',
  );
  shells.forEach((shell) => {
    const controller = controlRoomMediaControllers.get(shell);
    if (controller) result.push({ shell, controller });
  });
  return result;
}

function syncGlobalMuteButton(scope: ParentNode = document): void {
  const mounted = listMountedMediaControllers(scope);
  let canMute = false;
  for (const { controller } of mounted) {
    if (!controller.setMuted || !controller.isMuted) continue;
    canMute = true;
  }
  const muteToggleButton = scope.querySelector<HTMLButtonElement>(
    '[data-action="control-room-toggle-mute-all"]',
  );
  if (muteToggleButton) {
    muteToggleButton.disabled = !canMute;
    muteToggleButton.classList.toggle(
      "btn-disabled",
      muteToggleButton.disabled,
    );
    const label = controlRoomMuteIntent === "mute" ? "Unmute All" : "Mute All";
    const actionLabel =
      controlRoomMuteIntent === "mute" ? "Unmute all" : "Mute all";
    muteToggleButton.textContent = label;
    muteToggleButton.setAttribute(
      "aria-label",
      `${actionLabel} monitor previews`,
    );
  }
}

function syncGlobalPlaybackButton(scope: ParentNode = document): void {
  const mounted = listMountedMediaControllers(scope);
  const canTogglePlayback = mounted.some(
    ({ controller }) => !!controller.play || !!controller.pause,
  );
  const anyPlaying = mounted.some(
    ({ controller }) => controller.isPlaying?.() === true,
  );
  const playbackToggleButton = scope.querySelector<HTMLButtonElement>(
    '[data-action="control-room-toggle-playback-all"]',
  );
  if (playbackToggleButton) {
    playbackToggleButton.disabled = !canTogglePlayback;
    playbackToggleButton.classList.toggle(
      "btn-disabled",
      playbackToggleButton.disabled,
    );
    const label =
      controlRoomPlaybackIntent === "play" || anyPlaying
        ? "Pause All"
        : "Play All";
    const actionLabel =
      controlRoomPlaybackIntent === "play" || anyPlaying
        ? "Pause all"
        : "Play all";
    playbackToggleButton.textContent = label;
    playbackToggleButton.setAttribute(
      "aria-label",
      `${actionLabel} monitor previews`,
    );
  }
}

function syncGlobalMediaButtons(scope: ParentNode = document): void {
  syncGlobalPlaybackButton(scope);
  syncGlobalMuteButton(scope);
  syncCardPlaybackButtons(scope);
}

function buildLocalCard(pipe: PipelineView): ControlRoomCardDescriptor {
  const localPreviewUrl = buildInputPreviewUrl(pipe.id);
  const inputLive =
    pipe.input.status === "on" || pipe.input.status === "warning";
  return {
    id: `local:${pipe.id}`,
    title: "Local HLS",
    mediaUrl: inputLive ? localPreviewUrl : null,
    loadOnDemand: false,
    emptyMessage:
      pipe.input.status === "on"
        ? pipe.input.flapping
          ? "Publisher is reconnecting repeatedly. Preview may flicker until the ingest stabilizes."
          : "Waiting for the first HLS segments."
        : pipe.input.status === "warning" && pipe.input.disconnectGraceActive
          ? "Publisher recently dropped. Waiting for reconnect before grace expires."
          : pipe.input.status === "warning"
            ? "Waiting for the first HLS segments."
            : "Pipeline input is offline.",
    openUrl: localPreviewUrl,
    copyUrl: localPreviewUrl,
    editable: false,
    outputId: null,
    pipelineId: null,
    monitoringUrl: localPreviewUrl,
    statusLabel:
      pipe.input.status === "on"
        ? pipe.input.flapping
          ? "Flapping"
          : "Live"
        : pipe.input.status === "warning"
          ? pipe.input.disconnectGraceActive
            ? "Recovering"
            : "Unstable"
          : "Offline",
  };
}

function buildOutputCard(
  output: ControlRoomOutputOption,
): ControlRoomCardDescriptor {
  const monitoringUrl = output.monitoringUrl || null;
  const previewable = isPreviewableOutputStatus(output.status);
  const statusLabel = getOutputMonitorStatusLabel(output);
  return {
    id: `output:${output.outputId}`,
    title: output.outputName,
    mediaUrl: previewable ? monitoringUrl : null,
    loadOnDemand: true,
    emptyMessage: monitoringUrl
      ? previewable
        ? "Waiting for the monitor feed."
        : "Output is not running."
      : "Monitoring URL not set.",
    openUrl: toOpenableMonitoringUrl(monitoringUrl),
    copyUrl: monitoringUrl,
    editable: true,
    outputId: output.outputId,
    pipelineId: output.pipelineId,
    monitoringUrl,
    statusLabel,
  };
}

function buildEmptyCard(message: string): ControlRoomCardDescriptor {
  return {
    id: `empty:${message}`,
    title: "No Monitor",
    mediaUrl: null,
    loadOnDemand: false,
    emptyMessage: message,
    openUrl: null,
    copyUrl: null,
    editable: false,
    outputId: null,
    pipelineId: null,
    monitoringUrl: null,
  };
}

function getCardStatusToneClasses(
  statusLabel: string | null | undefined,
): string {
  switch ((statusLabel || "").trim().toLowerCase()) {
    case "live":
    case "forwarding":
      return "border-emerald-500/30 bg-emerald-500/[0.05]";
    case "awaiting keyframe":
    case "unstable":
    case "recovering":
      return "border-amber-500/30 bg-amber-500/[0.06]";
    case "down":
      return "border-rose-500/30 bg-rose-500/[0.05]";
    case "stopped":
    case "offline":
      return "border-base-content/10 bg-base-100";
    default:
      return "border-base-content/10 bg-base-100";
  }
}

function getStatusLabelClasses(statusLabel: string | null | undefined): string {
  switch ((statusLabel || "").trim().toLowerCase()) {
    case "live":
    case "forwarding":
      return "text-emerald-700 dark:text-emerald-300";
    case "awaiting keyframe":
    case "unstable":
    case "recovering":
      return "text-amber-700 dark:text-amber-300";
    case "down":
      return "text-rose-700 dark:text-rose-300";
    case "stopped":
    case "offline":
      return "text-base-content/45";
    default:
      return "text-base-content/45";
  }
}

function buildCardDescriptors(
  selectedPipeline: PipelineView | null,
  pipelineInputs: PipelineInput[] | null,
): ControlRoomCardDescriptor[] {
  if (!selectedPipeline) {
    return [
      buildEmptyCard(
        "Select a pipeline to load the local HLS preview and monitoring cards.",
      ),
    ];
  }

  const descriptors: ControlRoomCardDescriptor[] = pipelineInputs
    ? pipelineInputs.map(buildControlRoomInputCard)
    : [buildLocalCard(selectedPipeline)];
  const allMonitoringOutputs = listMonitoringOutputsForPipeline(
    selectedPipeline.id,
  );
  const outputs = filterMonitoringOutputs(
    allMonitoringOutputs,
    controlRoomState.searchQuery,
  );
  const start = controlRoomState.page * OUTPUTS_PER_PAGE;
  const pageOutputs = outputs.slice(start, start + OUTPUTS_PER_PAGE);

  if (pageOutputs.length === 0) {
    descriptors.push(
      buildEmptyCard(
        allMonitoringOutputs.length === 0
          ? "This pipeline does not have any monitoring URLs yet."
          : `No monitoring outputs match "${controlRoomState.searchQuery.trim()}". Clear search to show all monitoring cards.`,
      ),
    );
    return descriptors;
  }

  descriptors.push(...pageOutputs.map(buildOutputCard));
  return descriptors;
}

function ensureShell(container: HTMLElement): void {
  if (container.dataset.ready === "true") return;
  container.dataset.ready = "true";
  container.innerHTML = `
        <div class="space-y-5">
            <section class="border-base-content/10 from-base-200 via-base-200 to-base-100 rounded-2xl border bg-gradient-to-br p-4 shadow-sm">
                <div class="flex flex-wrap items-center justify-between gap-3">
                    <div>
                        <h1 class="text-lg font-semibold">Control Room</h1>
                        <p id="control-room-route-summary" class="text-base-content/60 mt-1 text-sm" role="status" aria-live="polite"></p>
                    </div>
                    <div class="flex flex-wrap items-center gap-2">
                        <button type="button" class="btn btn-sm btn-outline" data-action="control-room-toggle-playback-all">Play All</button>
                        <button type="button" class="btn btn-sm btn-outline" data-action="control-room-toggle-mute-all">Mute All</button>
                        <button type="button" id="control-room-reset-btn" class="btn btn-sm btn-outline" aria-label="Reset monitor wall">Reset</button>
                    </div>
                </div>
                <div class="mt-4 border-t border-base-content/10 pt-3">
                    <h2 id="control-room-controls-title" class="text-sm font-semibold tracking-[0.01em]">Monitor controls</h2>
                    <p class="text-base-content/60 mt-1 text-xs">Choose a pipeline, narrow the wall, and control all visible previews.</p>
                </div>
                <div class="mt-3 flex flex-wrap items-end gap-3" aria-labelledby="control-room-controls-title">
                    <label class="min-w-[18rem] flex-1 text-sm">
                        <span class="text-base-content/70 mb-1 block text-xs font-semibold uppercase">Pipeline</span>
                        <select id="control-room-pipeline-select" class="select select-sm w-full" aria-label="Filter monitor by pipeline"></select>
                    </label>
                    <label class="min-w-[12rem] flex-1 text-sm">
                        <span class="text-base-content/70 mb-1 block text-xs font-semibold uppercase">Search Outputs</span>
                        <input type="text" id="control-room-search-input" aria-label="Search monitor outputs" placeholder="Search outputs..." class="input input-sm input-bordered w-full" />
                    </label>
                    <div class="flex items-center gap-2">
                        <button type="button" class="btn btn-sm btn-outline" data-action="control-room-prev-page" aria-label="Previous monitor page">Prev</button>
                        <span id="control-room-page-label" class="text-base-content/70 min-w-[6rem] text-center text-sm">Page 1 / 1</span>
                        <button type="button" class="btn btn-sm btn-outline" data-action="control-room-next-page" aria-label="Next monitor page">Next</button>
                    </div>
                </div>
                <div class="text-base-content/60 mt-2 text-xs" id="control-room-summary" role="status" aria-live="polite"></div>
            </section>
            <section aria-labelledby="control-room-previews-title" class="space-y-3">
                <div class="flex flex-wrap items-end justify-between gap-3">
                    <div>
                        <h2 id="control-room-previews-title" class="text-sm font-semibold tracking-[0.01em]">Monitor previews</h2>
                        <p class="text-base-content/60 mt-1 text-xs">Local HLS first, followed by configured output monitors.</p>
                    </div>
                </div>
                <div id="control-room-grid" class="grid gap-4 sm:grid-cols-2 xl:grid-cols-4"></div>
            </section>
        </div>`;

  container.addEventListener("change", (event) => {
    const select = (event.target as Element | null)?.closest?.(
      "#control-room-pipeline-select",
    ) as HTMLSelectElement | null;
    if (!select) return;
    selectControlRoomPipeline(select.value || null);
    controlRoomCardActionsExpanded.clear();
    renderControlRoom();
  });

  container.addEventListener("input", (event) => {
    const input = (event.target as Element | null)?.closest?.(
      "#control-room-search-input",
    ) as HTMLInputElement | null;
    if (!input) return;
    controlRoomState.searchQuery = input.value || "";
    controlRoomState.page = 0;
    controlRoomCardActionsExpanded.clear();
    normalizeState();
    persistState();
    renderControlRoom();
  });

  container.addEventListener("click", async (event) => {
    const button = (event.target as Element | null)?.closest?.(
      "[data-action]",
    ) as HTMLButtonElement | null;
    if (!button) return;
    const action = button.dataset.action;
    if (action === "control-room-prev-page") {
      controlRoomState.page = Math.max(0, controlRoomState.page - 1);
      controlRoomCardActionsExpanded.clear();
      persistState();
      renderControlRoom();
      return;
    }
    if (action === "control-room-next-page") {
      controlRoomState.page += 1;
      controlRoomCardActionsExpanded.clear();
      normalizeState();
      persistState();
      renderControlRoom();
      return;
    }
    if (action === "control-room-clear-search") {
      controlRoomState.searchQuery = "";
      controlRoomState.page = 0;
      controlRoomCardActionsExpanded.clear();
      persistState();
      renderControlRoom();
      container
        .querySelector<HTMLInputElement>("#control-room-search-input")
        ?.focus();
      return;
    }
    if (action === "control-room-toggle-playback-all") {
      const mounted = listMountedMediaControllers(container);
      const shouldPause = controlRoomPlaybackIntent === "play";
      controlRoomPlaybackIntent = shouldPause ? "pause" : "play";
      mounted.forEach(({ controller }) => {
        if (shouldPause) {
          controller.pause?.();
        } else {
          controller.play?.();
        }
      });
      window.setTimeout(() => {
        syncGlobalPlaybackButton(container);
        syncCardPlaybackButtons(container);
      }, 0);
      return;
    }
    if (action === "control-room-toggle-mute-all") {
      const mounted = listMountedMediaControllers(container);
      const shouldMute = controlRoomMuteIntent !== "mute";
      controlRoomMuteIntent = shouldMute ? "mute" : "unmute";
      mounted.forEach(({ controller }) => {
        controller.setMuted?.(shouldMute);
      });
      syncGlobalMuteButton(container);
      return;
    }
    if (action === "control-room-copy-url") {
      const url = button.dataset.url || "";
      if (url && (await copyText(url))) showCopiedNotification();
      return;
    }
    if (action === "control-room-open-url") {
      const url = button.dataset.url || "";
      const title =
        button
          .closest("article")
          ?.querySelector<HTMLElement>('[data-role="control-room-title"]')
          ?.textContent?.trim() || "Monitor";
      if (url) openMonitorUrl(url, title);
      return;
    }
    if (action === "control-room-load-preview") {
      const cardId = button.closest<HTMLElement>("article")?.dataset.cardId;
      if (!cardId) return;
      controlRoomLoadedEmbedCards.add(cardId);
      renderControlRoom();
      return;
    }
    if (action === "control-room-promote-input") {
      const pipelineId = button.dataset.pipelineId || "";
      const inputId = button.dataset.inputId || "";
      if (!pipelineId || !inputId) return;
      await promoteControlRoomInput(pipelineId, inputId, renderControlRoom);
      return;
    }
    if (action === "control-room-toggle-card-actions") {
      const cardId = button.closest<HTMLElement>("article")?.dataset.cardId;
      if (!cardId) return;
      if (controlRoomCardActionsExpanded.has(cardId)) {
        controlRoomCardActionsExpanded.delete(cardId);
      } else {
        controlRoomCardActionsExpanded.add(cardId);
      }
      renderControlRoom();
      return;
    }
    if (action === "control-room-toggle-fullscreen") {
      const target = getMediaControllerForAction(button);
      if (!target) return;
      await requestMonitorFullscreen(target.shell);
      return;
    }
    if (action === "control-room-toggle-mute") {
      const target = getMediaControllerForAction(button);
      if (!target?.controller.setMuted || !target.controller.isMuted) return;
      const muted = target.controller.isMuted();
      target.controller.setMuted(!muted);
      controlRoomMuteIntent = !muted ? "mute" : "unmute";
      setMuteButtonLabel(button, !muted);
      syncGlobalMuteButton(container);
      return;
    }
    if (action === "control-room-toggle-playback") {
      const target = getMediaControllerForAction(button);
      if (
        !target?.controller.play ||
        !target.controller.pause ||
        !target.controller.isPlaying
      ) {
        return;
      }
      if (target.controller.isPlaying()) {
        target.controller.pause();
        controlRoomPlaybackIntent = "pause";
      } else {
        target.controller.play();
        controlRoomPlaybackIntent = "play";
      }
      window.setTimeout(() => {
        setPlaybackButtonLabel(
          button,
          target.controller.isPlaying?.() === true,
        );
        syncGlobalPlaybackButton(container);
      }, 0);
      return;
    }
    if (action === "control-room-edit-url") {
      const outputId = button.dataset.outputId || "";
      const output = findOutput(outputId);
      if (!outputId || !output) return;
      controlRoomMonitoringDrafts.set(outputId, output.monitoringUrl || "");
      pendingMonitoringInputFocusOutputId = outputId;
      renderControlRoom();
      return;
    }
    if (action === "control-room-cancel-url") {
      const outputId = button.dataset.outputId || "";
      if (!outputId) return;
      controlRoomMonitoringDrafts.delete(outputId);
      controlRoomMonitoringSavePending.delete(outputId);
      renderControlRoom();
      return;
    }
    if (action === "control-room-save-url") {
      const outputId = button.dataset.outputId || "";
      if (outputId) await saveMonitoringUrlFromControlRoom(outputId);
    }
  });

  container.addEventListener("input", (event) => {
    const input = (event.target as Element | null)?.closest?.(
      '[data-role="control-room-monitoring-input"]',
    ) as HTMLInputElement | null;
    const outputId = input?.dataset.outputId || "";
    if (!input || !outputId) return;
    controlRoomMonitoringDrafts.set(outputId, input.value);
    input.classList.remove("input-error");
  });

  container.addEventListener("keydown", async (event) => {
    const input = (event.target as Element | null)?.closest?.(
      '[data-role="control-room-monitoring-input"]',
    ) as HTMLInputElement | null;
    const outputId = input?.dataset.outputId || "";
    if (!input || !outputId) return;
    if (event.key === "Enter") {
      event.preventDefault();
      await saveMonitoringUrlFromControlRoom(outputId);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      controlRoomMonitoringDrafts.delete(outputId);
      controlRoomMonitoringSavePending.delete(outputId);
      renderControlRoom();
    }
  });

  container
    .querySelector<HTMLButtonElement>("#control-room-reset-btn")
    ?.addEventListener("click", () => {
      controlRoomState = {
        pipelineId: getDefaultPipelineId(),
        page: 0,
        searchQuery: "",
      };
      controlRoomCardActionsExpanded.clear();
      persistState();
      renderControlRoom();
    });
}

function clearCardPlayerShell(
  shell: HTMLElement | null,
  options: { resetMediaKey?: boolean } = {},
): void {
  if (!shell) return;
  controlRoomMediaControllers.get(shell)?.destroy();
  controlRoomMediaControllers.delete(shell);
  clearManagedHlsPlayer(
    shell.querySelector<HTMLElement>('[data-role="control-room-media-frame"]'),
  );
  shell.replaceChildren();
  if (options.resetMediaKey !== false) {
    delete shell.dataset.mediaKey;
  }
}

function setTileMessage(shell: HTMLElement, message: string): void {
  shell.innerHTML = `<div class="text-base-content/70 flex ${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} items-center justify-center px-4 py-5 text-center text-sm leading-6">${escapeHtml(message)}</div>`;
}

function setLazyEmbedMessage(shell: HTMLElement, message: string): void {
  const actionLabel = monitorPreviewActionLabel(shell, "Load preview");
  shell.innerHTML = `<div class="text-base-content/70 flex ${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} flex-col items-center justify-center gap-3 px-4 py-5 text-center text-sm leading-6">
        <span>${escapeHtml(message)}</span>
        <button type="button" class="btn btn-xs btn-accent btn-outline" data-action="control-room-load-preview" aria-label="${escapeHtml(actionLabel)}">Load preview</button>
    </div>`;
}

function isHlsMonitoringUrl(url: string): boolean {
  return /\.m3u8(?:$|[?#])/i.test(url);
}

function isDirectVideoMonitoringUrl(url: string): boolean {
  return /\.(mp4|m4v|webm|ogg|mov)(?:$|[?#])/i.test(url);
}

function isYouTubeMonitoringUrl(url: string): boolean {
  try {
    const parsed = new URL(url);
    const host = parsed.hostname.replace(/^www\./i, "").toLowerCase();
    return host === "youtu.be" || host.endsWith("youtube.com");
  } catch {
    return false;
  }
}

function applyYouTubeMonitoringParams(embed: URL): string {
  embed.searchParams.set("autoplay", "1");
  embed.searchParams.set("mute", "1");
  embed.searchParams.set("playsinline", "1");
  embed.searchParams.set("controls", "0");
  embed.searchParams.set("enablejsapi", "1");
  embed.searchParams.set("modestbranding", "1");
  embed.searchParams.set("disablekb", "1");
  embed.searchParams.set("fs", "0");
  embed.searchParams.set("iv_load_policy", "3");
  embed.searchParams.set("rel", "0");
  embed.searchParams.set("origin", window.location.origin);
  return embed.toString();
}

function toEmbeddableMonitoringUrl(url: string): string {
  try {
    const parsed = new URL(url);
    const host = parsed.hostname.replace(/^www\./i, "").toLowerCase();
    const pathParts = parsed.pathname.split("/").filter(Boolean);
    if (host === "youtu.be" && pathParts[0]) {
      const embed = new URL(
        `https://www.youtube-nocookie.com/embed/${encodeURIComponent(pathParts[0])}`,
      );
      return applyYouTubeMonitoringParams(embed);
    }
    if (host.endsWith("youtube.com")) {
      const videoId =
        parsed.searchParams.get("v") || pathParts[1] || pathParts[0] || "";
      if (parsed.pathname === "/watch" && videoId) {
        const embed = new URL(
          `https://www.youtube-nocookie.com/embed/${encodeURIComponent(videoId)}`,
        );
        return applyYouTubeMonitoringParams(embed);
      }
      if (
        (pathParts[0] === "live" ||
          pathParts[0] === "shorts" ||
          pathParts[0] === "embed") &&
        pathParts[1]
      ) {
        const embed = new URL(
          `https://www.youtube-nocookie.com/embed/${encodeURIComponent(pathParts[1])}`,
        );
        return applyYouTubeMonitoringParams(embed);
      }
    }
    return url;
  } catch {
    return url;
  }
}

function toOpenableMonitoringUrl(url: string | null): string | null {
  if (!url) return null;
  try {
    const parsed = new URL(url);
    const host = parsed.hostname.replace(/^www\./i, "").toLowerCase();
    const pathParts = parsed.pathname.split("/").filter(Boolean);
    if (host === "youtu.be" && pathParts[0]) {
      return `https://www.youtube.com/live/${encodeURIComponent(pathParts[0])}?feature=share`;
    }
    if (host.endsWith("youtube.com")) {
      const videoId =
        parsed.searchParams.get("v") || pathParts[1] || pathParts[0] || "";
      if (videoId) {
        return `https://www.youtube.com/live/${encodeURIComponent(videoId)}?feature=share`;
      }
    }
    return url;
  } catch {
    return url;
  }
}

function setCardWarning(shell: HTMLElement, message: string | null): void {
  const article = shell.closest("article");
  const cardId = article?.dataset.cardId || "";
  if (cardId) {
    if (message) {
      controlRoomCardWarnings.set(cardId, message);
    } else {
      controlRoomCardWarnings.delete(cardId);
    }
  }
  const warning = article?.querySelector<HTMLElement>(
    '[data-role="control-room-card-warning"]',
  );
  if (!message) {
    warning?.remove();
    return;
  }
  const status = article?.querySelector<HTMLElement>(
    '[data-role="control-room-card-status"]',
  );
  const statusCluster = article?.querySelector<HTMLElement>(
    '[data-role="control-room-card-status-cluster"]',
  );
  const target =
    warning ||
    (() => {
      const badge = document.createElement("div");
      badge.className =
        "inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border border-amber-500/35 bg-amber-500/12 text-xs font-bold text-amber-700 dark:text-amber-300";
      badge.dataset.role = "control-room-card-warning";
      badge.textContent = "!";
      if (status) {
        status.before(badge);
      } else {
        statusCluster?.appendChild(badge);
      }
      return badge;
    })();
  target.setAttribute("title", message);
  target.setAttribute("aria-label", message);
}

function getYouTubeMonitoringWarning(
  status: YoutubeMonitoringStatus | null,
): string | null {
  if (!status) return null;
  if (status.live_now) return null;
  return status.live_content || status.upcoming
    ? "This YouTube monitor is not live right now. Update the monitoring URL if the stream moved or has ended."
    : "This YouTube monitor resolves to a regular video, not a live stream. Update the monitoring URL to the active live share URL.";
}

async function fetchYouTubeMonitoringStatus(
  monitoringUrl: string,
): Promise<YoutubeMonitoringStatus | null> {
  const now = Date.now();
  const cached = youtubeMonitoringStatusCache.get(monitoringUrl);
  if (cached && cached.expiresAt > now) return cached.data;
  if (cached?.pending) return cached.pending;

  const pending = getYoutubeMonitoringStatus(monitoringUrl).then((data) => {
    youtubeMonitoringStatusCache.set(monitoringUrl, {
      expiresAt: Date.now() + YOUTUBE_MONITORING_STATUS_TTL_MS,
      data,
    });
    return data;
  });

  youtubeMonitoringStatusCache.set(monitoringUrl, {
    expiresAt: 0,
    data: cached?.data || null,
    pending,
  });
  return pending;
}

function refreshYouTubeCardWarning(
  shell: HTMLElement,
  monitoringUrl: string,
): void {
  void fetchYouTubeMonitoringStatus(monitoringUrl).then((status) => {
    if (!document.body.contains(shell)) return;
    // The shell is a reused DOM node: by the time this fetch resolves it may
    // have been reassigned to a different output/URL. Only apply the result
    // if the shell is still showing the media this fetch was started for,
    // otherwise a slow stale response can overwrite a newer card's status.
    if (shell.dataset.mediaKey !== monitoringUrl) return;
    setCardWarning(shell, getYouTubeMonitoringWarning(status));
  });
}

function buildMonitorPopupFeatures(): string {
  const width = Math.min(
    1600,
    Math.max(960, Math.floor(window.screen.availWidth * 0.86)),
  );
  const height = Math.min(
    1100,
    Math.max(720, Math.floor(window.screen.availHeight * 0.9)),
  );
  const left = Math.max(0, Math.floor((window.screen.availWidth - width) / 2));
  const top = Math.max(0, Math.floor((window.screen.availHeight - height) / 2));
  return `noopener,width=${width},height=${height},left=${left},top=${top}`;
}

function openSizedPopup(url: string): Window | null {
  return window.open(url, "_blank", buildMonitorPopupFeatures());
}

function openMonitorUrl(url: string, _title: string): void {
  openSizedPopup(url);
}

export function openOutputMonitoringUrl(url: string | null | undefined): void {
  const openUrl = toOpenableMonitoringUrl(url || null);
  if (!openUrl) return;
  openMonitorUrl(openUrl, "Monitor");
}

function createMonitorFrame(shell: HTMLElement): {
  frame: HTMLElement;
  controls: HTMLElement;
} {
  shell.innerHTML = "";

  const surface = document.createElement("div");
  surface.className = `${CONTROL_ROOM_MONITOR_FRAME_CLASS} ${CONTROL_ROOM_PLAYER_HEIGHT_CLASS}`;

  const frame = document.createElement("div");
  frame.dataset.role = "control-room-media-frame";
  frame.className = "h-full w-full";

  const topShade = document.createElement("div");
  topShade.className =
    "pointer-events-none absolute inset-x-0 top-0 h-14 bg-gradient-to-b from-black/45 to-transparent";

  const bottomShade = document.createElement("div");
  bottomShade.className =
    "pointer-events-none absolute inset-x-0 bottom-0 h-16 bg-gradient-to-t from-black/65 to-transparent";

  const controls = document.createElement("div");
  controls.dataset.role = "control-room-media-controls";
  controls.className =
    "absolute right-2 top-2 z-10 flex gap-1.5 opacity-0 transition-opacity duration-150 group-hover:opacity-100 group-focus-within:opacity-100";

  surface.appendChild(frame);
  surface.appendChild(topShade);
  surface.appendChild(bottomShade);
  surface.appendChild(controls);
  shell.appendChild(surface);
  return { frame, controls };
}

function addMonitorButton(
  controls: HTMLElement,
  action: string,
  label: string,
): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = CONTROL_ROOM_MONITOR_BUTTON_CLASS;
  button.dataset.action = action;
  button.textContent = label;
  controls.appendChild(button);
  button.setAttribute("aria-label", monitorPreviewActionLabel(button, label));
  return button;
}

function controlRoomCardTitle(element: Element): string {
  return (
    element.closest<HTMLElement>("article")?.dataset.cardTitle?.trim() || ""
  );
}

function monitorPreviewActionLabel(element: Element, label: string): string {
  const title = controlRoomCardTitle(element);
  if (!title) return label;
  return label.toLowerCase().includes("preview")
    ? `${label} for ${title}`
    : `${label} preview for ${title}`;
}

async function requestMonitorFullscreen(shell: HTMLElement): Promise<void> {
  const fullscreenTarget =
    shell.querySelector<HTMLElement>('[data-role="control-room-media-frame"]')
      ?.parentElement || shell;
  if (!document.fullscreenElement) {
    await fullscreenTarget.requestFullscreen?.();
    return;
  }
  if (document.fullscreenElement === fullscreenTarget) {
    await document.exitFullscreen?.();
  } else {
    await fullscreenTarget.requestFullscreen?.();
  }
}

function setMuteButtonLabel(button: HTMLButtonElement, muted: boolean): void {
  const label = muted ? "Unmute" : "Mute";
  button.textContent = label;
  button.setAttribute("aria-label", monitorPreviewActionLabel(button, label));
}

function setPlaybackButtonLabel(
  button: HTMLButtonElement,
  playing: boolean,
): void {
  const label = playing ? "Pause" : "Play";
  button.textContent = label;
  button.setAttribute("aria-label", monitorPreviewActionLabel(button, label));
}

function syncCardPlaybackButtons(scope: ParentNode = document): void {
  listMountedMediaControllers(scope).forEach(({ shell, controller }) => {
    const button = shell.querySelector<HTMLButtonElement>(
      '[data-action="control-room-toggle-playback"]',
    );
    if (
      !button ||
      !controller.play ||
      !controller.pause ||
      !controller.isPlaying
    ) {
      return;
    }
    setPlaybackButtonLabel(button, controller.isPlaying());
  });
}

function registerMediaController(
  shell: HTMLElement,
  controller: ControlRoomMediaController,
): void {
  controlRoomMediaControllers.set(shell, controller);
  if (controller.setMuted) {
    controller.setMuted(controlRoomMuteIntent === "mute");
  }
  if (controlRoomPlaybackIntent === "play") {
    controller.play?.();
  } else {
    controller.pause?.();
  }
}

function getMediaControllerForAction(
  target: Element | null,
): { shell: HTMLElement; controller: ControlRoomMediaController } | null {
  const shell = target?.closest?.(
    '[data-role="control-room-player-shell"]',
  ) as HTMLElement | null;
  if (!shell) return null;
  const controller = controlRoomMediaControllers.get(shell);
  if (!controller) return null;
  return { shell, controller };
}

function loadYouTubeIframeApi(): Promise<YouTubeApiNamespace> {
  if (window.YT?.Player) {
    return Promise.resolve(window.YT);
  }
  if (youtubeIframeApiPromise) return youtubeIframeApiPromise;

  youtubeIframeApiPromise = new Promise((resolve, reject) => {
    const existingScript = document.querySelector<HTMLScriptElement>(
      'script[data-role="youtube-iframe-api"]',
    );
    const cleanup = () => {
      if (window.onYouTubeIframeAPIReady === handleReady) {
        window.onYouTubeIframeAPIReady = undefined;
      }
    };
    const handleReady = () => {
      cleanup();
      if (window.YT?.Player) {
        resolve(window.YT);
        return;
      }
      reject(new Error("YouTube iframe API loaded without Player"));
    };

    window.onYouTubeIframeAPIReady = handleReady;

    if (!existingScript) {
      const script = document.createElement("script");
      script.src = "https://www.youtube.com/iframe_api";
      script.async = true;
      script.dataset.role = "youtube-iframe-api";
      script.addEventListener("error", () => {
        cleanup();
        reject(new Error("Failed to load YouTube iframe API"));
      });
      document.head.appendChild(script);
      return;
    }

    existingScript.addEventListener("error", () => {
      cleanup();
      reject(new Error("Failed to load YouTube iframe API"));
    });
  });

  return youtubeIframeApiPromise;
}

function detectMonitoringEmbedKind(url: string): MonitoringEmbedKind {
  if (/^srt:\/\//i.test(url)) return "unsupported";
  if (isHlsMonitoringUrl(url)) return "hls";
  if (isDirectVideoMonitoringUrl(url)) return "video";
  if (isYouTubeMonitoringUrl(url)) return "youtube";
  if (/^https?:\/\//i.test(url)) return "iframe";
  return "unsupported";
}

function syncCardMedia(
  cardId: string,
  shell: HTMLElement,
  mediaUrl: string | null,
  loadOnDemand: boolean,
  emptyMessage: string,
): void {
  if (!mediaUrl) {
    const desiredKey = `message:${emptyMessage}`;
    if (shell.dataset.mediaKey === desiredKey) return;
    clearCardPlayerShell(shell, { resetMediaKey: false });
    shell.dataset.mediaKey = desiredKey;
    setTileMessage(shell, emptyMessage);
    return;
  }

  const embedKind = detectMonitoringEmbedKind(mediaUrl);
  const isWaitingForPreview =
    loadOnDemand && !controlRoomLoadedEmbedCards.has(cardId);
  const desiredKey = isWaitingForPreview ? `lazy:${mediaUrl}` : mediaUrl;
  if (shell.dataset.mediaKey === desiredKey) return;
  clearCardPlayerShell(shell, { resetMediaKey: false });
  shell.dataset.mediaKey = desiredKey;

  if (embedKind === "unsupported") {
    setTileMessage(
      shell,
      "This URL is saved, but this card can only preview browser-playable sources today.",
    );
    return;
  }
  if (isWaitingForPreview) {
    setLazyEmbedMessage(shell, "Preview is not loaded yet.");
    return;
  }

  if (embedKind === "hls" || embedKind === "video") {
    const { frame, controls } = createMonitorFrame(shell);
    const playbackButton = addMonitorButton(
      controls,
      "control-room-toggle-playback",
      "Play",
    );
    const muteButton = addMonitorButton(
      controls,
      "control-room-toggle-mute",
      "Unmute",
    );
    addMonitorButton(controls, "control-room-toggle-fullscreen", "Fullscreen");

    if (embedKind === "hls") {
      renderManagedHlsPlayer(frame, mediaUrl, {
        className: `${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} w-full bg-black object-contain`,
        loadingLabel: "Loading...",
        idleLabel: "Paused",
        autoStart: false,
        showOverlayButton: false,
        controls: false,
      });
      const managedController = getManagedHlsController(frame);
      const video = frame.querySelector<HTMLVideoElement>(
        '[data-role="managed-hls-video"]',
      );
      if (!managedController || !video) return;
      registerMediaController(shell, {
        destroy: () => clearManagedHlsPlayer(frame),
        play: () => managedController.play(),
        pause: () => managedController.pause(),
        isPlaying: () => managedController.isPlaying(),
        isMuted: () => managedController.isMuted(),
        setMuted: (muted: boolean) => managedController.setMuted(muted),
      });
      setMuteButtonLabel(muteButton, video.muted);
      setPlaybackButtonLabel(playbackButton, managedController.isPlaying());
      video.addEventListener("play", () => {
        setPlaybackButtonLabel(playbackButton, true);
        syncGlobalPlaybackButton(document);
      });
      video.addEventListener("pause", () => {
        setPlaybackButtonLabel(playbackButton, false);
        syncGlobalPlaybackButton(document);
      });
    } else {
      const video = document.createElement("video");
      video.className = `${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} w-full bg-black object-contain`;
      video.controls = false;
      video.setAttribute("controlslist", "nodownload");
      video.autoplay = true;
      video.muted = true;
      video.playsInline = true;
      frame.appendChild(video);
      video.src = mediaUrl;
      void video.play().catch(() => {
        // Autoplay can be blocked; controls remain available.
      });
      registerMediaController(shell, {
        destroy: () => {
          video.pause();
          video.removeAttribute("src");
          video.load();
        },
        play: () => {
          void video.play().catch(() => {
            // Autoplay can still be denied until the browser is ready.
          });
        },
        pause: () => {
          video.pause();
        },
        isPlaying: () => !video.paused && !video.ended,
        isMuted: () => video.muted,
        setMuted: (muted: boolean) => {
          video.muted = muted;
        },
      });
      setMuteButtonLabel(muteButton, video.muted);
      setPlaybackButtonLabel(playbackButton, !video.paused && !video.ended);
      video.addEventListener("play", () => {
        setPlaybackButtonLabel(playbackButton, true);
        syncGlobalPlaybackButton(document);
      });
      video.addEventListener("pause", () => {
        setPlaybackButtonLabel(playbackButton, false);
        syncGlobalPlaybackButton(document);
      });
    }
    return;
  }

  if (embedKind === "youtube") {
    const { frame, controls } = createMonitorFrame(shell);
    const playbackButton = addMonitorButton(
      controls,
      "control-room-toggle-playback",
      "Play",
    );
    playbackButton.disabled = true;
    playbackButton.classList.add("btn-disabled");
    const cardId = shell.closest("article")?.dataset.cardId || "";
    setCardWarning(shell, controlRoomCardWarnings.get(cardId) || null);
    const muteButton = addMonitorButton(
      controls,
      "control-room-toggle-mute",
      "Unmute",
    );
    muteButton.disabled = true;
    muteButton.classList.add("btn-disabled");
    addMonitorButton(controls, "control-room-toggle-fullscreen", "Fullscreen");

    const iframeWrap = document.createElement("div");
    iframeWrap.className = "pointer-events-none absolute inset-[-7%]";

    const iframe = document.createElement("iframe");
    const iframeId = `control-room-youtube-${Math.random().toString(36).slice(2, 10)}`;
    iframe.id = iframeId;
    iframe.src = toEmbeddableMonitoringUrl(mediaUrl);
    iframe.className = "h-full w-full border-0 bg-black";
    iframe.allow =
      "autoplay; clipboard-write; encrypted-media; picture-in-picture; web-share";
    iframe.referrerPolicy = "strict-origin-when-cross-origin";
    iframe.loading = "lazy";
    iframe.title = "Monitoring player";
    iframe.setAttribute("allowfullscreen", "true");
    iframeWrap.appendChild(iframe);
    frame.className = `${frame.className} relative`;
    frame.appendChild(iframeWrap);

    let player: YouTubePlayerApi | null = null;
    let disposed = false;
    registerMediaController(shell, {
      destroy: () => {
        disposed = true;
        player?.destroy();
      },
      play: () => {
        player?.playVideo();
      },
      pause: () => {
        player?.pauseVideo();
      },
      isPlaying: () => player?.getPlayerState?.() === 1,
      isMuted: () => player?.isMuted() ?? true,
      setMuted: (muted: boolean) => {
        if (!player) return;
        if (muted) {
          player.mute();
        } else {
          player.unMute();
        }
      },
    });

    void loadYouTubeIframeApi()
      .then((YT) => {
        if (disposed) return;
        player = new YT.Player(iframeId, {
          events: {
            onReady: () => {
              if (!player) return;
              player.mute();
              setPlaybackButtonLabel(
                playbackButton,
                player.getPlayerState?.() === 1,
              );
              playbackButton.disabled = false;
              playbackButton.classList.remove("btn-disabled");
              setMuteButtonLabel(muteButton, true);
              muteButton.disabled = false;
              muteButton.classList.remove("btn-disabled");
              refreshYouTubeCardWarning(shell, mediaUrl);
            },
            onStateChange: () => {
              if (!player) return;
              setPlaybackButtonLabel(
                playbackButton,
                player.getPlayerState?.() === 1,
              );
              syncGlobalPlaybackButton(document);
            },
          },
        });
      })
      .catch(() => {
        playbackButton.disabled = true;
        playbackButton.classList.add("btn-disabled");
        playbackButton.textContent = "Unavailable";
        muteButton.disabled = true;
        muteButton.classList.add("btn-disabled");
        muteButton.textContent = "Unavailable";
      });
    return;
  }

  if (embedKind === "iframe") {
    const { frame, controls } = createMonitorFrame(shell);
    addMonitorButton(controls, "control-room-toggle-fullscreen", "Fullscreen");
    const iframe = document.createElement("iframe");
    iframe.src = toEmbeddableMonitoringUrl(mediaUrl);
    iframe.className = `${CONTROL_ROOM_PLAYER_HEIGHT_CLASS} w-full border-0 bg-black`;
    iframe.allow =
      "autoplay; clipboard-write; encrypted-media; picture-in-picture; web-share";
    iframe.referrerPolicy = "strict-origin-when-cross-origin";
    iframe.loading = "lazy";
    iframe.title = "Monitoring player";
    iframe.setAttribute("allowfullscreen", "true");
    frame.appendChild(iframe);
    registerMediaController(shell, {
      destroy: () => {
        iframe.src = "about:blank";
      },
    });
    return;
  }

  setTileMessage(shell, emptyMessage);
}

function ensureCardElements(grid: HTMLElement, cardCount: number): void {
  while (grid.children.length > cardCount) {
    const child = grid.lastElementChild as HTMLElement | null;
    if (!child) break;
    clearCardPlayerShell(
      child.querySelector<HTMLElement>(
        '[data-role="control-room-player-shell"]',
      ),
    );
    child.remove();
  }

  while (grid.children.length < cardCount) {
    const article = document.createElement("article");
    article.className = `${CONTROL_ROOM_CARD_BASE_CLASS} border-base-content/10 bg-base-100`;
    article.innerHTML = `
            <div class="min-w-0" data-role="control-room-title"></div>
            <div class="mt-2 min-h-[1.75rem] min-w-0" data-role="control-room-details"></div>
            <div class="border-base-content/10 bg-base-200/70 mt-3 min-w-0 overflow-hidden rounded-[1rem] border p-1" data-role="control-room-player-shell"></div>`;
    grid.appendChild(article);
  }
}

function syncCard(
  article: HTMLElement,
  descriptor: ControlRoomCardDescriptor,
): void {
  const previousId = article.dataset.cardId || "";
  if (previousId && previousId !== descriptor.id) {
    controlRoomCardWarnings.delete(previousId);
    controlRoomCardActionsExpanded.delete(previousId);
    clearCardPlayerShell(
      article.querySelector<HTMLElement>(
        '[data-role="control-room-player-shell"]',
      ),
    );
  }
  article.dataset.cardId = descriptor.id;
  article.dataset.cardTitle = descriptor.title;

  const title = article.querySelector<HTMLElement>(
    '[data-role="control-room-title"]',
  );
  const details = article.querySelector<HTMLElement>(
    '[data-role="control-room-details"]',
  );
  const playerShell = article.querySelector<HTMLElement>(
    '[data-role="control-room-player-shell"]',
  );
  if (!title || !details || !playerShell) return;

  article.className = `${CONTROL_ROOM_CARD_BASE_CLASS} ${getCardStatusToneClasses(descriptor.statusLabel)}`;
  const statusLabel = descriptor.statusLabel
    ? `<div class="${getStatusLabelClasses(descriptor.statusLabel)} shrink-0 text-[10px] font-medium uppercase tracking-[0.14em]" data-role="control-room-card-status">${escapeHtml(descriptor.statusLabel)}</div>`
    : "";
  title.innerHTML = `
        <div class="flex items-start justify-between gap-2">
            <div class="min-w-0">
                <h3 class="truncate text-sm font-semibold tracking-[0.01em]">${escapeHtml(descriptor.title)}</h3>
                ${descriptor.subtitle ? `<p class="text-base-content/55 mt-1 truncate text-xs">${escapeHtml(descriptor.subtitle)}</p>` : ""}
            </div>
            <div class="flex shrink-0 items-center gap-1.5" data-role="control-room-card-status-cluster">
                ${statusLabel}
            </div>
        </div>`;
  const isEditing =
    !!descriptor.outputId &&
    controlRoomMonitoringDrafts.has(descriptor.outputId);
  const isSaving =
    !!descriptor.outputId &&
    controlRoomMonitoringSavePending.has(descriptor.outputId);

  if (isEditing && descriptor.outputId) {
    const draftValue =
      controlRoomMonitoringDrafts.get(descriptor.outputId) ??
      descriptor.monitoringUrl ??
      "";
    details.innerHTML = `
            <label class="flex flex-col gap-1">
                <span class="text-base-content/55 text-[11px] font-medium uppercase tracking-[0.14em]">Monitoring URL</span>
                <div class="flex items-center gap-2">
                    <input
                        type="text"
                        class="input input-bordered input-xs min-w-0 flex-1"
                        data-role="control-room-monitoring-input"
                        data-output-id="${escapeHtml(descriptor.outputId)}"
                        value="${escapeHtml(draftValue)}"
                        placeholder="https://example.com/live/master.m3u8"
                        ${isSaving ? "disabled" : ""}
                    />
                    <button
                        type="button"
                        class="btn btn-xs btn-accent"
                        data-action="control-room-save-url"
                        data-output-id="${escapeHtml(descriptor.outputId)}"
                        ${isSaving ? "disabled" : ""}>
                        ${isSaving ? "Saving" : "Save"}
                    </button>
                    <button
                        type="button"
                        class="btn btn-xs btn-ghost"
                        data-action="control-room-cancel-url"
                        data-output-id="${escapeHtml(descriptor.outputId)}"
                        ${isSaving ? "disabled" : ""}>
                        Cancel
                    </button>
                </div>
            </label>`;

    if (pendingMonitoringInputFocusOutputId === descriptor.outputId) {
      window.setTimeout(() => {
        const input = article.querySelector<HTMLInputElement>(
          '[data-role="control-room-monitoring-input"]',
        );
        input?.focus();
        input?.select();
      }, 0);
      pendingMonitoringInputFocusOutputId = null;
    }
  } else {
    const hasCardActions =
      descriptor.editable ||
      !!descriptor.copyUrl ||
      !!descriptor.openUrl ||
      !!descriptor.promoteInputId;
    const cardActionsExpanded =
      !controlRoomV2Active() ||
      !hasCardActions ||
      controlRoomCardActionsExpanded.has(descriptor.id);
    const editButton = descriptor.editable
      ? `
                <button
                    type="button"
                    class="btn btn-xs btn-outline"
                    data-action="control-room-edit-url"
                    aria-label="Edit monitoring URL for ${escapeHtml(descriptor.title)}"
                    data-output-id="${escapeHtml(descriptor.outputId || "")}">
                    Edit
                </button>`
      : "";
    const promotionPending =
      !!descriptor.promoteInputId &&
      isControlRoomInputPromotionPending(descriptor.promoteInputId);
    const promoteButton = descriptor.promoteInputId
      ? `
                <button
                    type="button"
                    class="btn btn-xs btn-accent"
                    data-action="control-room-promote-input"
                    data-pipeline-id="${escapeHtml(descriptor.pipelineId || "")}"
                    data-input-id="${escapeHtml(descriptor.promoteInputId)}"
                    ${promotionPending ? "disabled" : ""}>
                    ${promotionPending ? "Promoting" : "Promote"}
                </button>`
      : "";
    const copyDisabled = descriptor.copyUrl ? "" : " disabled";
    const openDisabled = descriptor.openUrl ? "" : " disabled";
    const actionButtons = `
            <div class="flex min-w-0 flex-wrap gap-1.5">
                ${promoteButton}
                ${editButton}
                <button
                    type="button"
                    class="btn btn-xs btn-outline"
                    data-action="control-room-copy-url"
                    aria-label="Copy monitoring URL for ${escapeHtml(descriptor.title)}"
                    data-url="${escapeHtml(descriptor.copyUrl || "")}"${copyDisabled}>
                    Copy
                </button>
                <button
                    type="button"
                    class="btn btn-xs btn-outline"
                    data-action="control-room-open-url"
                    aria-label="Open monitor for ${escapeHtml(descriptor.title)}"
                    data-url="${escapeHtml(descriptor.openUrl || "")}"${openDisabled}>
                    Open
                </button>
            </div>`;
    if (controlRoomV2Active() && hasCardActions) {
      details.innerHTML = `
            <div class="min-w-0 space-y-2">
                <button
                    type="button"
                    class="btn btn-xs btn-outline"
                    data-action="control-room-toggle-card-actions"
                    aria-label="${cardActionsExpanded ? "Hide" : "Show"} monitor actions for ${escapeHtml(descriptor.title)}"
                    aria-expanded="${cardActionsExpanded ? "true" : "false"}">
                    ${cardActionsExpanded ? "Hide monitor actions" : "Show monitor actions"}
                </button>
                ${
                  cardActionsExpanded
                    ? actionButtons
                    : `<p class="text-base-content/55 text-xs">Preview/status stays visible; URL actions are tucked away until needed.</p>`
                }
            </div>`;
    } else {
      details.innerHTML = `
            <div class="min-w-0">
                ${actionButtons}
            </div>`;
    }
  }

  const warning = controlRoomCardWarnings.get(descriptor.id) || null;
  setCardWarning(playerShell, warning);
  if (
    descriptor.monitoringUrl &&
    isYouTubeMonitoringUrl(descriptor.monitoringUrl)
  ) {
    refreshYouTubeCardWarning(playerShell, descriptor.monitoringUrl);
  }

  syncCardMedia(
    descriptor.id,
    playerShell,
    descriptor.mediaUrl,
    descriptor.loadOnDemand,
    descriptor.emptyMessage,
  );
}

function renderPipelineSelect(
  container: HTMLElement,
  pipelines: PipelineView[],
): void {
  const select = container.querySelector<HTMLSelectElement>(
    "#control-room-pipeline-select",
  );
  if (!select) return;
  const options = pipelines
    .map((pipe) => {
      const selected =
        pipe.id === controlRoomState.pipelineId ? " selected" : "";
      return `<option value="${escapeHtml(pipe.id)}"${selected}>${escapeHtml(pipe.name)}</option>`;
    })
    .join("");
  select.innerHTML = options || '<option value="">No pipelines</option>';
  select.value = controlRoomState.pipelineId || "";
  select.disabled = pipelines.length === 0;
}

function filterMonitoringOutputs(
  outputs: ControlRoomOutputOption[],
  searchQuery: string,
): ControlRoomOutputOption[] {
  const q = searchQuery.toLowerCase().trim();
  if (!q) return outputs;
  return outputs.filter((out) =>
    getMonitoringOutputSearchText(out).includes(q),
  );
}

function getOutputMonitorStatusLabel(output: ControlRoomOutputOption): string {
  const normalizedStatus = (output.status || "off").trim().toLowerCase();
  return output.flapping &&
    (normalizedStatus === "running" ||
      normalizedStatus === "on" ||
      normalizedStatus === "warning")
    ? "Flapping"
    : normalizedStatus === "running" || normalizedStatus === "on"
      ? "Live"
      : normalizedStatus === "retrying"
        ? "Recovering"
        : normalizedStatus === "warning"
          ? "Unstable"
          : normalizedStatus === "failed"
            ? "Down"
            : "Stopped";
}

function getMonitoringOutputSearchText(
  output: ControlRoomOutputOption,
): string {
  const normalizedStatus = (output.status || "").trim().toLowerCase();
  const statusLabel = getOutputMonitorStatusLabel(output).toLowerCase();
  const statusAliases = new Set<string>();
  if (normalizedStatus) statusAliases.add(normalizedStatus);
  if (statusLabel) statusAliases.add(statusLabel);
  if (output.flapping) statusAliases.add("flapping");
  if (statusLabel === "live") statusAliases.add("running");
  if (statusLabel === "down") statusAliases.add("failed");
  if (statusLabel === "recovering") statusAliases.add("retrying");
  if (statusLabel === "stopped") {
    statusAliases.add("off");
    statusAliases.add("offline");
  }
  return [
    output.outputName,
    output.monitoringUrl || "",
    output.pipelineName,
    ...statusAliases,
  ]
    .join(" ")
    .toLowerCase();
}

function renderControlRoomCheckpointPresentation(
  selectedPipeline: PipelineView | null,
): void {
  const allMonitoringOutputs = selectedPipeline
    ? listMonitoringOutputsForPipeline(selectedPipeline.id)
    : [];
  const filteredMonitoringOutputs = filterMonitoringOutputs(
    allMonitoringOutputs,
    controlRoomState.searchQuery,
  );
  const lazyWebPreviewCount = allMonitoringOutputs.filter(
    (output) =>
      output.monitoringUrl &&
      detectMonitoringEmbedKind(output.monitoringUrl) === "iframe",
  ).length;
  controlRoomCheckpointCallback?.(
    buildControlRoomCheckpointModel({
      allMonitoringOutputs,
      filteredMonitoringOutputCount: filteredMonitoringOutputs.length,
      lazyWebPreviewCount,
      searchQuery: controlRoomState.searchQuery,
      selectedPipeline,
    }),
  );
}

function renderControlRoomScopeSummary(
  container: HTMLElement,
  selectedPipeline: PipelineView | null,
): void {
  const summary = container.querySelector<HTMLElement>(
    "#control-room-route-summary",
  );
  if (!summary) return;
  summary.textContent = controlRoomScopeSummaryText(
    selectedPipeline,
    selectedPipeline
      ? listMonitoringOutputsForPipeline(selectedPipeline.id).length
      : 0,
  );
}

function renderSummaryAndPagination(
  container: HTMLElement,
  selectedPipeline: PipelineView | null,
): void {
  const summary = container.querySelector<HTMLElement>("#control-room-summary");
  const pageLabel = container.querySelector<HTMLElement>(
    "#control-room-page-label",
  );
  const prevButton = container.querySelector<HTMLButtonElement>(
    '[data-action="control-room-prev-page"]',
  );
  const nextButton = container.querySelector<HTMLButtonElement>(
    '[data-action="control-room-next-page"]',
  );
  if (!summary || !pageLabel || !prevButton || !nextButton) return;

  if (!selectedPipeline) {
    summary.textContent = "No pipelines available yet.";
    pageLabel.textContent = "Page 1 / 1";
    prevButton.disabled = true;
    nextButton.disabled = true;
    prevButton.classList.add("btn-disabled");
    nextButton.classList.add("btn-disabled");
    return;
  }

  const totalOutputs = selectedPipeline.outs.length;
  const allMonitoringOutputs = listMonitoringOutputsForPipeline(
    selectedPipeline.id,
  );
  const monitoringOutputs = filterMonitoringOutputs(
    allMonitoringOutputs,
    controlRoomState.searchQuery,
  );
  const missingMonitoring = totalOutputs - allMonitoringOutputs.length;
  const totalPages = Math.max(
    1,
    Math.ceil(monitoringOutputs.length / OUTPUTS_PER_PAGE),
  );
  pageLabel.textContent = `Page ${controlRoomState.page + 1} / ${totalPages}`;
  prevButton.disabled = controlRoomState.page === 0;
  nextButton.disabled = controlRoomState.page >= totalPages - 1;
  prevButton.classList.toggle("btn-disabled", prevButton.disabled);
  nextButton.classList.toggle("btn-disabled", nextButton.disabled);
  const query = controlRoomState.searchQuery.trim();
  summary.textContent = query
    ? `${monitoringOutputs.length}/${allMonitoringOutputs.length} monitored match · ${missingMonitoring} missing monitoring URLs · "${query}"`
    : `${allMonitoringOutputs.length}/${totalOutputs} monitored · ${missingMonitoring} missing monitoring URLs`;
}

function renderControlRoom(): void {
  const container = document.getElementById("control-mode-content");
  if (!container) return;

  ensureStateLoaded();
  syncControlRoomWorkspaceSelection();
  persistState();
  ensureShell(container);

  const pipelines = listPipelines();
  const selectedPipeline =
    pipelines.find((pipe) => pipe.id === controlRoomState.pipelineId) || null;
  const pipelineInputs = selectedPipeline
    ? controlRoomInputs(selectedPipeline.id, renderControlRoom)
    : null;

  renderPipelineSelect(container, pipelines);
  renderControlRoomScopeSummary(container, selectedPipeline);
  renderControlRoomCheckpointPresentation(selectedPipeline);

  // Sync search input value
  const searchInput = container.querySelector<HTMLInputElement>(
    "#control-room-search-input",
  );
  if (searchInput && searchInput.value !== controlRoomState.searchQuery) {
    searchInput.value = controlRoomState.searchQuery;
  }
  let clearSearchButton = container.querySelector<HTMLButtonElement>(
    "#control-room-clear-search-btn",
  );
  if (controlRoomState.searchQuery.trim()) {
    if (!clearSearchButton) {
      clearSearchButton = document.createElement("button");
      clearSearchButton.id = "control-room-clear-search-btn";
      clearSearchButton.type = "button";
      clearSearchButton.className = "btn btn-sm btn-outline";
      clearSearchButton.dataset.action = "control-room-clear-search";
      clearSearchButton.setAttribute("aria-label", "Clear monitor search");
      clearSearchButton.textContent = "Clear search";
      searchInput?.closest("label")?.after(clearSearchButton);
    }
  } else {
    clearSearchButton?.remove();
  }

  renderSummaryAndPagination(container, selectedPipeline);

  const grid = container.querySelector<HTMLElement>("#control-room-grid");
  if (!grid) return;

  const descriptors = buildCardDescriptors(selectedPipeline, pipelineInputs);
  ensureCardElements(grid, descriptors.length);
  descriptors.forEach((descriptor, index) => {
    const article = grid.children[index] as HTMLElement | undefined;
    if (article) syncCard(article, descriptor);
  });
  syncGlobalMediaButtons(container);
}

function findOutput(outputId: string): OutputView | null {
  for (const pipe of state.pipelines) {
    const output = pipe.outs.find((candidate) => candidate.id === outputId);
    if (output) return output;
  }
  return null;
}

async function saveMonitoringUrlFromControlRoom(
  outputId: string,
): Promise<void> {
  if (!outputId || controlRoomMonitoringSavePending.has(outputId)) return;

  const pipeline = state.pipelines.find((pipe) =>
    pipe.outs.some((candidate) => candidate.id === outputId),
  );
  const output =
    pipeline?.outs.find((candidate) => candidate.id === outputId) || null;
  if (!pipeline || !output) {
    showErrorAlert("Output not found");
    return;
  }

  const input = document.querySelector<HTMLInputElement>(
    `[data-role="control-room-monitoring-input"][data-output-id="${CSS.escape(outputId)}"]`,
  );
  const monitoringUrl = (
    input?.value ??
    controlRoomMonitoringDrafts.get(outputId) ??
    ""
  ).trim();
  controlRoomMonitoringDrafts.set(outputId, monitoringUrl);

  if (monitoringUrl && !isValidMonitoringUrl(monitoringUrl)) {
    input?.classList.add("input-error");
    input?.focus();
    showErrorAlert(
      "Monitoring URL must start with http://, https://, or srt://",
    );
    return;
  }

  input?.classList.remove("input-error");
  controlRoomMonitoringSavePending.add(outputId);
  renderControlRoom();

  try {
    const res = await updateOutput(pipeline.id, output.id, {
      name: output.name,
      config: normalizeOutputConfig(output),
      url: output.url,
      monitoringUrl,
    });
    if (res === null) return;
    controlRoomMonitoringDrafts.delete(outputId);
    upsertDashboardOutputConfig(res.output);
  } finally {
    controlRoomMonitoringSavePending.delete(outputId);
    renderControlRoom();
  }
}

export function openControlRoomForOutput(outputId: string): void {
  const output = findOutput(outputId);
  const pipeline = state.pipelines.find((pipe) =>
    pipe.outs.some((candidate) => candidate.id === outputId),
  );
  if (!pipeline) {
    workspaceDependencies.openMonitorView(null);
    renderControlRoom();
    return;
  }

  const monitoringOutputs = listMonitoringOutputsForPipeline(pipeline.id);
  const outputIndex = monitoringOutputs.findIndex(
    (candidate) => candidate.outputId === outputId,
  );
  controlRoomState = {
    pipelineId: pipeline.id,
    page: outputIndex >= 0 ? Math.floor(outputIndex / OUTPUTS_PER_PAGE) : 0,
    searchQuery: "",
  };
  if (output && !output.monitoringUrl) controlRoomState.page = 0;
  persistState();
  workspaceDependencies.openMonitorView(pipeline.id);
  renderControlRoom();
}

export { renderControlRoom, refreshYouTubeCardWarning };
