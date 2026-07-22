import {
  copyText,
  isValidMonitoringUrl,
  showCopiedNotification,
  showErrorAlert,
  escapeHtml,
} from "../../core/utils.js";
import { updateOutput } from "../../core/api.js";
import { RenderScope } from "../../core/render-scope.js";
import type { RenderScopeToken } from "../../core/render-scope.js";
import { state } from "../../core/state.js";
import { controlRoomShellHtml } from "./shell.js";
import { upsertDashboardOutputConfig } from "../dashboard.js";
import type { OutputView, PipelineInput, PipelineView } from "../../types.js";
import { normalizeOutputConfig } from "../../core/output-config.js";
import type { ControlRoomCheckpointModel } from "./view-model.js";
import {
  buildControlRoomCheckpointModel,
  controlRoomScopeSummaryText,
} from "./checkpoint.js";
import type {
  ControlRoomCardDescriptor,
  ControlRoomOutputOption,
  ControlRoomState,
  ControlRoomWorkspaceDependencies,
} from "./types.js";
import {
  buildControlRoomInputCard,
  controlRoomInputs,
  isControlRoomInputPromotionPending,
  promoteControlRoomInput,
} from "./inputs.js";
import {
  controlRoomCardWarnings,
  controlRoomLoadedEmbedCards,
  listMountedMediaControllers,
  openMonitorUrl,
  requestMonitorFullscreen,
  setMuteButtonLabel,
  setPlaybackButtonLabel,
  syncCardPlaybackButtons,
  syncGlobalMuteButton,
  syncGlobalPlaybackButton,
  getMediaControllerForAction,
  syncGlobalMediaButtons,
} from "./monitor.js";
import {
  buildEmptyCard,
  buildLocalCard,
  buildOutputCard,
  configureControlRoomV2Presentation,
  ensureCardElements,
  getOutputMonitorStatusLabel,
  isPreviewableOutputStatus,
  syncCard,
} from "./cards.js";

const CONTROL_ROOM_STATE_KEY = "dashboard:control-room-state";
const OUTPUTS_PER_PAGE = 11;

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
const controlRoomScope = new RenderScope("control-mode-content");
import {
  controlRoomCardActionsExpanded,
  controlRoomMonitoringDrafts,
  controlRoomMonitoringSavePending,
  controlRoomMuteIntent,
  controlRoomPlaybackIntent,
  pendingMonitoringInputFocusOutputId,
  setControlRoomMuteIntent,
  setControlRoomPlaybackIntent,
  setPendingMonitoringInputFocusOutputId,
} from "./state.js";
let controlRoomCheckpointCallback:
  ((model: ControlRoomCheckpointModel | null) => void) | null = null;
const controlRoomNameCollator = new Intl.Collator(undefined, {
  numeric: true,
  sensitivity: "base",
});

export {
  controlRoomCardActionsExpanded,
  controlRoomMonitoringDrafts,
  controlRoomMonitoringSavePending,
  controlRoomMuteIntent,
  controlRoomPlaybackIntent,
  pendingMonitoringInputFocusOutputId,
};

function renderControlRoomIfCurrent(token: RenderScopeToken): void {
  if (controlRoomScope.isCurrent(token)) renderControlRoom();
}

export function configureControlRoomCheckpointPresentation(options: {
  readonly onPresentation?: (model: ControlRoomCheckpointModel | null) => void;
  readonly v2Active?: boolean;
}): void {
  configureControlRoomV2Presentation({ active: options.v2Active === true });
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
  container.innerHTML = controlRoomShellHtml();

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
      setControlRoomPlaybackIntent(shouldPause ? "pause" : "play");
      for (const { controller } of mounted) {
        if (shouldPause) {
          controller.pause?.();
        } else {
          controller.play?.();
        }
      }
      return;
    }
    if (action === "control-room-toggle-mute-all") {
      const mounted = listMountedMediaControllers(container);
      const anyUnmuted = mounted.some(
        ({ controller }) => controller.isMuted?.() === false,
      );
      const shouldMute = anyUnmuted || controlRoomMuteIntent === "unmute";
      setControlRoomMuteIntent(shouldMute ? "mute" : "unmute");
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
      const token = controlRoomScope.token();
      await promoteControlRoomInput(pipelineId, inputId, () =>
        renderControlRoomIfCurrent(token),
      );
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
      setControlRoomMuteIntent(!muted ? "mute" : "unmute");
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
        setControlRoomPlaybackIntent("pause");
      } else {
        target.controller.play();
        setControlRoomPlaybackIntent("play");
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
      setPendingMonitoringInputFocusOutputId(outputId);
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
      output.monitoringUrl && /^https?:\/\//i.test(output.monitoringUrl),
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
  const token = controlRoomScope.token();
  ensureStateLoaded();
  syncControlRoomWorkspaceSelection();
  persistState();

  const pipelines = listPipelines();
  const selectedPipeline =
    pipelines.find((pipe) => pipe.id === controlRoomState.pipelineId) || null;
  renderControlRoomCheckpointPresentation(selectedPipeline);
  const container = document.getElementById(controlRoomScope.current());
  if (!container) return;
  ensureShell(container);
  const pipelineInputs = selectedPipeline
    ? controlRoomInputs(selectedPipeline.id, () =>
        renderControlRoomIfCurrent(token),
      )
    : null;

  renderPipelineSelect(container, pipelines);
  renderControlRoomScopeSummary(container, selectedPipeline);

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
  setPendingMonitoringInputFocusOutputId(null);
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
  const token = controlRoomScope.token();
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
    renderControlRoomIfCurrent(token);
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

function setControlRoomContainerId(containerId: string): void {
  controlRoomScope.setContainerId(containerId);
}

export {
  renderControlRoom,
  setControlRoomContainerId,
};
export { openOutputMonitoringUrl, refreshYouTubeCardWarning } from "./monitor.js";
