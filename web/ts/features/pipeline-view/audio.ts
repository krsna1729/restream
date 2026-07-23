import { formatChannelCount, formatCodecName } from "../../core/utils.js";
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
let audioTrackStateChangeHandler: (() => void) | null = null;

export function setAudioTrackStateChangeHandler(
  handler: (() => void) | null,
): void {
  audioTrackStateChangeHandler = handler;
}

function notifyAudioTrackStateChanged(): void {
  audioTrackStateChangeHandler?.();
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
  notifyAudioTrackStateChanged();
}

export function updatePipelineAudioTrackDraft(
  pipelineId: string,
  key: string,
  value: string,
): void {
  if (!resolveAudioTrack(pipelineId, key)) return;
  audioLabelDrafts.set(`${pipelineId}:${key}`, value);
  notifyAudioTrackStateChanged();
}

export function cancelPipelineAudioTrackEdit(
  pipelineId: string,
  key: string,
): void {
  const editKey = `${pipelineId}:${key}`;
  audioLabelEditKeys.delete(editKey);
  audioLabelDrafts.delete(editKey);
  notifyAudioTrackStateChanged();
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
  notifyAudioTrackStateChanged();
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

// ── Format helpers used only within this module ────────────────────────

function formatSampleRate(value: number | null | undefined): string {
  if (!Number.isFinite(value) || (value as number) <= 0) return "--";
  const khz = (value as number) / 1000;
  return `${Number.isInteger(khz) ? khz : khz.toFixed(1)} kHz`;
}
