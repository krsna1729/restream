// What the Media workspace shows about the media directory: which files are
// recordings and which are sources, how each one is stored, and whether a
// file can feed a live pipeline as it is.

import type { MediaFile, MediaFileAnalysis } from "../core/api-types.js";
import type { PipelineDraft } from "./manage-model.js";

export type MediaKind = "recording" | "source";

/** The engine's kind when it reports one; otherwise a file some pipeline
 * ingests is a source, and a name with "recording" in it is a recording. */
export function mediaKind(file: MediaFile): MediaKind {
  if (file.kind) return file.kind;
  if ((file.ingestCount ?? 0) > 0) return "source";
  return file.name.toLowerCase().includes("recording") ? "recording" : "source";
}

/** What a library row is selected by: a recording's session id, which stays
 * the same when its `.ts` becomes the finalized `.mp4`; else its name. */
export function mediaKey(file: MediaFile): string {
  return file.recordingId ? `recording:${file.recordingId}` : file.name;
}

/** The files whose name (or source or converted name) holds `query`,
 * ignoring case; all of them when the query is blank. */
export function searchMedia(
  files: readonly MediaFile[],
  query: string,
): readonly MediaFile[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return files;
  return files.filter((file) =>
    [file.name, file.sourceName, file.convertedName].some((name) =>
      name?.toLowerCase().includes(needle),
    ),
  );
}

/** Recordings and sources, newest first in each. */
export function splitLibrary(files: readonly MediaFile[]): {
  readonly recordings: readonly MediaFile[];
  readonly sources: readonly MediaFile[];
} {
  const newest = [...files].sort(
    (a, b) => Date.parse(b.modifiedAt) - Date.parse(a.modifiedAt) || a.name.localeCompare(b.name),
  );
  return {
    recordings: newest.filter((file) => mediaKind(file) === "recording"),
    sources: newest.filter((file) => mediaKind(file) === "source"),
  };
}

/** The names of a file's stored forms: the original, and the converted MP4
 * when one exists apart from it. */
export function storedForms(file: MediaFile): {
  readonly original: string;
  readonly converted: string | null;
} {
  const original = file.sourceName || file.name;
  const converted =
    file.convertedName && file.convertedName !== original ? file.convertedName : null;
  return { original, converted };
}

export type Readiness =
  | { readonly kind: "ready"; readonly detail: string }
  | { readonly kind: "needsReencode"; readonly detail: string }
  | { readonly kind: "unknown"; readonly detail: string }
  | { readonly kind: "noVideo"; readonly detail: string };

/**
 * Whether a file can feed a live pipeline as it is. A sparse GOP (a gap
 * between keyframes longer than the live target, within the engine's
 * tolerance) plays only through a Live Optimized re-encode. The engine
 * measures spacing only between two keyframes or more, so a file with
 * fewer is judged here: none, or one in a file longer than the target,
 * leaves a late joiner without a keyframe.
 */
export function ingestReadiness(analysis: MediaFileAnalysis): Readiness {
  const target = analysis.liveGopTargetSeconds;
  const reencode = `use it with Live Optimized, which re-encodes toward ${target} s`;
  if (!analysis.videoCodec) {
    return { kind: "noVideo", detail: "No video stream was found in this file." };
  }
  const max = analysis.maxKeyframeIntervalSec;
  if (max === null || max === undefined) {
    if (analysis.keyframeCount === 0) {
      return { kind: "needsReencode", detail: `No keyframe was found: ${reencode}.` };
    }
    const duration = analysis.durationSec;
    if (analysis.keyframeCount === 1 && duration != null && duration > target) {
      return {
        kind: "needsReencode",
        detail: `Only one keyframe in ${formatSeconds(duration)}: ${reencode}.`,
      };
    }
    return {
      kind: "unknown",
      detail: "The keyframe spacing could not be measured.",
    };
  }
  if (analysis.sparseForLive) {
    return {
      kind: "needsReencode",
      detail: `Keyframes are up to ${formatSeconds(max)} apart, more than the ${target} s live target: ${reencode}.`,
    };
  }
  return {
    kind: "ready",
    detail: `Keyframes are at most ${formatSeconds(max)} apart, within the engine's tolerance of the ${target} s live target: no re-encode is needed.`,
  };
}

function formatSeconds(value: number): string {
  return `${value.toFixed(2)} s`;
}

/** `h:mm:ss`, or `m:ss` under an hour. */
export function formatDuration(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined || !Number.isFinite(seconds)) return "--";
  const total = Math.round(seconds);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

export interface TimelineEvent {
  /** `null`: the step has no recorded time (it is under way, or the engine
   * did not keep one). */
  readonly atMs: number | null;
  readonly label: string;
  readonly tone: "ok" | "info" | "error";
}

function instant(value: string | null | undefined): number | null {
  const ms = value ? Date.parse(value) : Number.NaN;
  return Number.isFinite(ms) ? ms : null;
}

/**
 * A recording's life, from the persisted session behind the file and its
 * conversion state; `null` for a file no recording session wrote. What the
 * recorder writes: the session start; at stop, only the `converting`
 * conversion state (its time is the stop); after the MP4 remux, the session
 * `ready` with `endedAt` = when the MP4 was written. A failed remux marks
 * both the session and the conversion failed while the `.ts` stays, so it
 * is one remux failure, not a failed recording.
 */
export function recordingTimeline(file: MediaFile): readonly TimelineEvent[] | null {
  const started = instant(file.recordingStartedAt);
  if (!file.recordingId || started === null) return null;
  const events: TimelineEvent[] = [
    {
      atMs: started,
      label: `Recording started${file.recordingCodecSummary ? ` · ${file.recordingCodecSummary}` : ""}`,
      tone: "ok",
    },
  ];
  const converted = instant(file.conversionUpdatedAt);
  if (file.conversionStatus === "converting") {
    events.push({ atMs: converted, label: "Stopped; writing the browser MP4", tone: "info" });
  } else if (file.conversionStatus === "failed") {
    events.push({
      atMs: converted,
      label: `MP4 remux failed: ${file.conversionError ?? "no reason was recorded"}; the .ts is kept`,
      tone: "error",
    });
  } else if (file.recordingStatus === "recording") {
    events.push({ atMs: null, label: "Still recording", tone: "info" });
  } else if (file.recordingStatus === "ready") {
    events.push({ atMs: instant(file.recordingEndedAt), label: "Stopped; MP4 written", tone: "ok" });
  } else if (file.recordingStatus === "failed") {
    events.push({
      atMs: instant(file.recordingEndedAt),
      label: `Recording failed: ${file.recordingError ?? "no reason was recorded"}`,
      tone: "error",
    });
  }
  return events;
}

/**
 * A new pipeline that plays `file`: named after it, looping, and through
 * Live Optimized when the file's keyframes are too sparse to join live.
 * `null` when the file cannot feed a pipeline (it has no video).
 */
export function sourceDraft(
  file: MediaFile,
  readiness: Readiness | null,
): Partial<PipelineDraft> | null {
  if (readiness?.kind === "noVideo") return null;
  const stem = file.name.replace(/\.[^.]+$/, "");
  return {
    name: stem || file.name,
    source: "file",
    filename: file.name,
    loop: true,
    liveOptimized: readiness?.kind === "needsReencode",
  };
}
