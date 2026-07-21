import {
  escapeHtml,
  formatChannelCount,
  formatCodecName,
  msToHHMMSS,
} from "../../core/utils.js";
import { state } from "../../core/state.js";
import {
  audioTrackKey,
  getAudioTrackLabel,
  getAudioTrackStoredLabel,
  setAudioTrackStoredLabel,
} from "../audio-track-labels.js";
import { clearInputPreview, renderInputPreview } from "../input-preview.js";
import type { AudioTrack, PipelineView } from "../../types.js";
import type { PipelineOperateAudioTrackModel } from "../pipeline-operate-view-model.js";

// ── Audio track editing state ──────────────────────────────────────────

const audioLabelEditKeys = new Set<string>();
const audioLabelDrafts = new Map<string, string>();
const expandedAudioTrackLists = new Set<string>();
let pendingAudioLabelFocusKey: string | null = null;
const AUDIO_TRACK_EXPANSION_STORAGE_KEY =
  "restream.audioTrackExpansion.v1";

function loadAudioTrackExpansionState(): void {
  expandedAudioTrackLists.clear();
  try {
    const raw = window.localStorage.getItem(AUDIO_TRACK_EXPANSION_STORAGE_KEY);
    const parsed = raw ? JSON.parse(raw) : [];
    if (!Array.isArray(parsed)) return;
    for (const key of parsed) {
      if (typeof key === "string" && key.trim()) {
        expandedAudioTrackLists.add(key);
      }
    }
  } catch {
    // Expansion persistence is a convenience; rendering should never depend on it.
  }
}

function persistAudioTrackExpansionState(): void {
  try {
    window.localStorage.setItem(
      AUDIO_TRACK_EXPANSION_STORAGE_KEY,
      JSON.stringify([...expandedAudioTrackLists]),
    );
  } catch {
    // Ignore storage failures so the dashboard remains usable.
  }
}

function audioTrackExpansionKey(pipelineId: string): string {
  return pipelineId;
}

// ── SVG icon helper ────────────────────────────────────────────────────

function editIconSvg(): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" class="h-3.5 w-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2" aria-hidden="true">
        <path stroke-linecap="round" stroke-linejoin="round" d="m16.862 4.487 1.687-1.688a1.875 1.875 0 1 1 2.652 2.652L10.582 16.07a4.5 4.5 0 0 1-1.897 1.13L6 18l.8-2.685a4.5 4.5 0 0 1 1.13-1.897l8.932-8.931Z" />
        <path stroke-linecap="round" stroke-linejoin="round" d="M19.5 7.125 16.875 4.5" />
    </svg>`;
}

// ── Formatting helpers ─────────────────────────────────────────────────

function formatAudioTrackIdentity(track: AudioTrack, label: string): string {
  const parts: string[] = [];
  if (Number.isFinite(track.pid as number)) {
    parts.push(`PID 0x${Number(track.pid).toString(16).toUpperCase()}`);
  }
  if (
    track.language?.trim() &&
    track.language.trim().toUpperCase() !== label.trim().toUpperCase()
  ) {
    parts.push(track.language.trim().toUpperCase());
  }
  return parts.join(" / ") || "Metadata";
}

export function buildAudioTrackModels(
  pipelineId: string,
  tracks: readonly AudioTrack[],
): PipelineOperateAudioTrackModel[] {
  return tracks.map((track, index) => {
    const key = audioTrackKey(track, index);
    const editKey = `${pipelineId}:${key}`;
    const label = getAudioTrackLabel(pipelineId, track, index);
    return {
      key,
      index,
      label,
      identity: formatAudioTrackIdentity(track, label),
      codec: formatCodecName(track.codec) || track.codec || "--",
      sampleRate: formatSampleRate(track.sample_rate),
      channels:
        track.channels !== null && track.channels !== undefined
          ? formatChannelCount(track.channels)
          : "--",
      profile: track.profile || "--",
      editing: audioLabelEditKeys.has(editKey),
      draft:
        audioLabelDrafts.get(editKey) ??
        getAudioTrackStoredLabel(pipelineId, track, index),
    };
  });
}

// ── Resolution helpers ─────────────────────────────────────────────────

function resolveAudioTrack(pipelineId: string, key: string) {
  const pipeline = state.pipelines.find(({ id }) => id === pipelineId);
  const index = pipeline?.input.audioTracks.findIndex(
    (track, position) => audioTrackKey(track, position) === key,
  );
  if (!pipeline || index === undefined || index < 0) return null;
  return { pipeline, track: pipeline.input.audioTracks[index], index };
}

// ── Exported editing API ───────────────────────────────────────────────

export function editPipelineAudioTrack(pipelineId: string, key: string): void {
  const resolved = resolveAudioTrack(pipelineId, key);
  if (!resolved) return;
  const editKey = `${pipelineId}:${key}`;
  audioLabelEditKeys.add(editKey);
  audioLabelDrafts.set(
    editKey,
    getAudioTrackStoredLabel(pipelineId, resolved.track, resolved.index),
  );
  renderAudioTracksTable(pipelineId);
}

export function updatePipelineAudioTrackDraft(
  pipelineId: string,
  key: string,
  value: string,
): void {
  if (!resolveAudioTrack(pipelineId, key)) return;
  audioLabelDrafts.set(`${pipelineId}:${key}`, value);
}

export function cancelPipelineAudioTrackEdit(
  pipelineId: string,
  key: string,
): void {
  const editKey = `${pipelineId}:${key}`;
  audioLabelEditKeys.delete(editKey);
  audioLabelDrafts.delete(editKey);
  renderAudioTracksTable(pipelineId);
}

export function savePipelineAudioTrack(
  pipelineId: string,
  key: string,
): void {
  const resolved = resolveAudioTrack(pipelineId, key);
  if (!resolved) return;
  const editKey = `${pipelineId}:${key}`;
  setAudioTrackStoredLabel(
    pipelineId,
    resolved.track,
    resolved.index,
    audioLabelDrafts.get(editKey) || "",
  );
  audioLabelEditKeys.delete(editKey);
  audioLabelDrafts.delete(editKey);
  renderAudioTracksTable(pipelineId);
}

// ── Input preview ───────────────────────────────────────────────────────

export function mountPipelineInputPreview(
  pipelineId: string,
  container: HTMLElement,
): void {
  const pipeline = state.pipelines.find(({ id }) => id === pipelineId);
  if (!pipeline || pipeline.input.status === "off") {
    clearInputPreview(container);
    return;
  }
  renderInputPreview(container, pipeline);
}

export function clearPipelineInputPreview(container: HTMLElement): void {
  clearInputPreview(container);
}

// ── Shared format helpers ───────────────────────────────────────────────

export function formatShortDurationMs(
  value: number | null | undefined,
): string {
  if (!Number.isFinite(value) || (value as number) < 0) return "--";
  const totalSeconds = Math.round((value as number) / 1000);
  if (totalSeconds < 60) return `${totalSeconds}s`;
  return msToHHMMSS(totalSeconds * 1000) || "--";
}

// ── Audio tracks table rendering ────────────────────────────────────────

export function renderAudioTracksTable(
  pipelineId: string,
  tracks?: AudioTrack[],
): void {
  if (!tracks) {
    const pipeline = state.pipelines.find((p) => p.id === pipelineId);
    tracks = pipeline?.input.audioTracks || [];
  }
  const audioTracksContainer = document.getElementById("input-audio-tracks");
  if (!audioTracksContainer) return;
  loadAudioTrackExpansionState();
  const expansionKey = audioTrackExpansionKey(pipelineId);
  const existingDetails = audioTracksContainer.querySelector<HTMLDetailsElement>(
    "details[data-audio-track-expansion-key]",
  );
  if (existingDetails) {
    if (existingDetails.open) {
      expandedAudioTrackLists.add(expansionKey);
    } else {
      expandedAudioTrackLists.delete(expansionKey);
    }
  }

  const activeInput =
    document.activeElement instanceof HTMLInputElement &&
    audioTracksContainer.contains(document.activeElement)
      ? document.activeElement
      : null;
  const activeEditKey = activeInput?.dataset.audioLabelEditKey || null;
  const activeSelectionStart = activeInput?.selectionStart ?? null;
  const activeSelectionEnd = activeInput?.selectionEnd ?? null;
  if (activeEditKey && activeInput) {
    audioLabelDrafts.set(activeEditKey, activeInput.value);
  }

  if (tracks.length === 0) {
    expandedAudioTrackLists.delete(expansionKey);
    persistAudioTrackExpansionState();
    audioTracksContainer.innerHTML =
      '<div class="stats border-base-content/10 bg-base-100 w-full border"><div class="stat p-3"><div class="stat-title">Audio</div><div class="stat-value text-sm">No tracks</div></div></div>';
    return;
  }

  const renderTrack = (track: AudioTrack, index: number): string => {
    const codec = formatCodecName(track.codec) || track.codec || "--";
    const label = getAudioTrackLabel(pipelineId, track, index);
    const storedLabel = getAudioTrackStoredLabel(pipelineId, track, index);
    const identity = formatAudioTrackIdentity(track, label);
    const key = audioTrackKey(track, index);
    const editKey = `${pipelineId}:${key}`;
    const isEditing = audioLabelEditKeys.has(editKey);
    const draftLabel = audioLabelDrafts.get(editKey) ?? storedLabel;
    const channelLabel =
      track.channels !== null && track.channels !== undefined
        ? formatChannelCount(track.channels)
        : "--";
    const trackStat = isEditing
      ? `<div class="stat min-w-0 place-items-center p-2 text-center">
                    <div class="stat-title">Track ${index + 1}</div>
                    <input
                        type="text"
                        class="input input-bordered input-xs mt-1 w-full max-w-44 text-center"
                        data-audio-label-input="${escapeHtml(key)}"
                        data-audio-label-index="${index}"
                        data-audio-label-edit-key="${escapeHtml(editKey)}"
                        value="${escapeHtml(draftLabel)}"
                        placeholder="${escapeHtml(label)}"
                        aria-label="Audio track name"
                    />
                    <div class="mt-1 flex justify-center gap-1">
                        <button type="button" class="btn btn-xs btn-accent" data-audio-label-action="save" data-audio-label-index="${index}">Save</button>
                        <button type="button" class="btn btn-xs btn-ghost" data-audio-label-action="cancel" data-audio-label-index="${index}">Cancel</button>
                    </div>
                </div>`
      : `<div class="stat relative min-w-0 place-items-center p-2 text-center">
                    <button
                        type="button"
                        class="btn btn-xs btn-ghost btn-square absolute top-1 right-1 h-6 min-h-0 w-6 opacity-70 hover:opacity-100"
                        data-audio-label-action="edit"
                        data-audio-label-index="${index}"
                        title="Rename track"
                        aria-label="Rename ${escapeHtml(label)}">
                        ${editIconSvg()}
                    </button>
                    <div class="stat-title">Track ${index + 1}</div>
                    <div class="stat-value truncate text-sm">${escapeHtml(label)}</div>
                    <div class="stat-desc truncate">${escapeHtml(identity)}</div>
                </div>`;

    return `<div class="stats border-base-content/10 bg-base-100 grid w-full grid-cols-2 overflow-hidden border sm:grid-cols-[minmax(0,1.15fr)_minmax(4rem,.65fr)_minmax(5rem,.8fr)_minmax(6rem,.95fr)_minmax(4rem,.65fr)]">
                ${trackStat}
                <div class="stat min-w-0 place-items-center p-2 text-center">
                    <div class="stat-title">Codec</div>
                    <div class="stat-value truncate text-sm">${escapeHtml(codec)}</div>
                </div>
                <div class="stat min-w-0 place-items-center p-2 text-center">
                    <div class="stat-title">Freq</div>
                    <div class="stat-value truncate text-sm">${escapeHtml(formatSampleRate(track.sample_rate))}</div>
                </div>
                <div class="stat min-w-0 place-items-center p-2 text-center">
                    <div class="stat-title">Channels</div>
                    <div class="stat-value truncate text-sm">${escapeHtml(channelLabel)}</div>
                </div>
                <div class="stat min-w-0 place-items-center p-2 text-center">
                    <div class="stat-title">Profile</div>
                    <div class="stat-value truncate text-sm">${escapeHtml(track.profile || "--")}</div>
                </div>
            </div>`;
  };

  const visibleLimit = tracks.length > 8 ? 6 : tracks.length;
  const visibleTracks = tracks
    .slice(0, visibleLimit)
    .map((track, index) => renderTrack(track, index))
    .join("");
  const extraTracks = tracks
    .slice(visibleLimit)
    .map((track, offset) => renderTrack(track, visibleLimit + offset))
    .join("");
  const extraTracksOpen = expandedAudioTrackLists.has(expansionKey);

  audioTracksContainer.innerHTML = `${visibleTracks}
    ${
      extraTracks
        ? `<details class="border-base-content/10 bg-base-100 rounded-lg border p-2" data-audio-track-expansion-key="${escapeHtml(expansionKey)}"${extraTracksOpen ? " open" : ""}>
            <summary class="cursor-pointer px-2 py-1 text-sm font-semibold">
              ${tracks.length - visibleLimit} more audio tracks
            </summary>
            <div class="mt-2 space-y-1">${extraTracks}</div>
          </details>`
        : ""
    }`;

  audioTracksContainer
    .querySelectorAll<HTMLDetailsElement>(
      "details[data-audio-track-expansion-key]",
    )
    .forEach((details) => {
      details.addEventListener("toggle", () => {
        const key = details.dataset.audioTrackExpansionKey || expansionKey;
        if (details.open) {
          expandedAudioTrackLists.add(key);
        } else {
          expandedAudioTrackLists.delete(key);
        }
        persistAudioTrackExpansionState();
      });
    });

  audioTracksContainer
    .querySelectorAll<HTMLButtonElement>("button[data-audio-label-action]")
    .forEach((button) => {
      const index = Number(button.dataset.audioLabelIndex);
      if (!Number.isFinite(index)) return;
      const track = tracks[index];
      const editKey = `${pipelineId}:${audioTrackKey(track, index)}`;
      button.addEventListener("click", () => {
        const action = button.dataset.audioLabelAction;
        if (action === "edit") {
          audioLabelEditKeys.add(editKey);
          audioLabelDrafts.set(
            editKey,
            getAudioTrackStoredLabel(pipelineId, track, index),
          );
          pendingAudioLabelFocusKey = editKey;
        } else if (action === "cancel") {
          audioLabelEditKeys.delete(editKey);
          audioLabelDrafts.delete(editKey);
        } else if (action === "save") {
          const input = audioTracksContainer.querySelector<HTMLInputElement>(
            `input[data-audio-label-index="${index}"]`,
          );
          setAudioTrackStoredLabel(
            pipelineId,
            track,
            index,
            audioLabelDrafts.get(editKey) ?? input?.value ?? "",
          );
          audioLabelEditKeys.delete(editKey);
          audioLabelDrafts.delete(editKey);
        }
        renderAudioTracksTable(pipelineId, tracks);
      });
    });
  audioTracksContainer
    .querySelectorAll<HTMLInputElement>("input[data-audio-label-index]")
    .forEach((input) => {
      const index = Number(input.dataset.audioLabelIndex);
      if (!Number.isFinite(index)) return;
      const editKey = `${pipelineId}:${audioTrackKey(tracks[index], index)}`;
      input.addEventListener("input", () => {
        audioLabelDrafts.set(editKey, input.value);
      });
      input.addEventListener("keydown", (event) => {
        if (event.key === "Enter") {
          setAudioTrackStoredLabel(
            pipelineId,
            tracks[index],
            index,
            audioLabelDrafts.get(editKey) ?? input.value,
          );
          audioLabelEditKeys.delete(editKey);
          audioLabelDrafts.delete(editKey);
          renderAudioTracksTable(pipelineId, tracks);
        }
        if (event.key === "Escape") {
          audioLabelEditKeys.delete(editKey);
          audioLabelDrafts.delete(editKey);
          renderAudioTracksTable(pipelineId, tracks);
        }
      });
    });

  const focusKey = activeEditKey || pendingAudioLabelFocusKey;
  if (focusKey) {
    const input = audioTracksContainer.querySelector<HTMLInputElement>(
      `input[data-audio-label-edit-key="${CSS.escape(focusKey)}"]`,
    );
    if (input) {
      input.focus();
      if (
        activeEditKey === focusKey &&
        activeSelectionStart !== null &&
        activeSelectionEnd !== null
      ) {
        input.setSelectionRange(activeSelectionStart, activeSelectionEnd);
      } else {
        input.select();
      }
    }
  }
  pendingAudioLabelFocusKey = null;
}

// ── Format helpers used only within this module ────────────────────────

function formatSampleRate(value: number | null | undefined): string {
  if (!Number.isFinite(value) || (value as number) <= 0) return "--";
  const khz = (value as number) / 1000;
  return `${Number.isInteger(khz) ? khz : khz.toFixed(1)} kHz`;
}
