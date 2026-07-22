import { escapeHtml } from "../../core/utils.js";
import type { PipelineInput, PipelineView } from "../../types.js";
import type {
  ControlRoomCardDescriptor,
  ControlRoomOutputOption,
} from "./types.js";
import { buildInputPreviewUrl } from "../input-preview.js";
import {
  buildControlRoomInputCard,
  isControlRoomInputPromotionPending,
} from "./inputs.js";
import {
  clearCardPlayerShell,
  controlRoomCardWarnings,
  isYouTubeMonitoringUrl,
  refreshYouTubeCardWarning,
  setCardWarning,
  syncCardMedia,
  toOpenableMonitoringUrl,
} from "./monitor.js";
import {
  controlRoomCardActionsExpanded,
  controlRoomMonitoringDrafts,
  controlRoomMonitoringSavePending,
  pendingMonitoringInputFocusOutputId,
} from "./state.js";

const CONTROL_ROOM_CARD_BASE_CLASS =
  "group flex min-h-[17rem] min-w-0 w-full max-w-full flex-col overflow-hidden rounded-2xl border p-3 shadow-[0_18px_45px_rgba(15,23,42,0.12)]";
let controlRoomV2PresentationActive = false;

export function configureControlRoomV2Presentation(options: {
  readonly active: boolean;
}): void {
  controlRoomV2PresentationActive = options.active;
}

function controlRoomV2Active(): boolean {
  return controlRoomV2PresentationActive;
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

function buildLocalCard(pipe: PipelineView): ControlRoomCardDescriptor {
  const localPreviewUrl = buildInputPreviewUrl(pipe.id);
  const inputLive =
    pipe.input.status === "on" || pipe.input.status === "warning";
  return {
    id: `local:${pipe.id}`,
    title: "Local HLS",
    mediaUrl: inputLive ? localPreviewUrl : null,
    loadOnDemand: true,
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

function isPreviewableOutputStatus(
  status: string | null | undefined,
): boolean {
  const normalized = (status || "").trim().toLowerCase();
  return (
    normalized === "on" || normalized === "running" || normalized === "warning"
  );
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
    !!descriptor.outputId && controlRoomMonitoringDrafts.has(descriptor.outputId);
  const isSaving =
    !!descriptor.outputId && controlRoomMonitoringSavePending.has(descriptor.outputId);

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

export {
  buildEmptyCard,
  buildLocalCard,
  buildOutputCard,
  ensureCardElements,
  getOutputMonitorStatusLabel,
  isPreviewableOutputStatus,
  syncCard,
};
