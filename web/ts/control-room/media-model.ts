// What the Media workspace shows about the media directory: which files are
// recordings and which are sources, how each one is stored, and whether a
// file can feed a live pipeline as it is.

import type { MediaFile, MediaFileAnalysis } from "../core/api-types.js";

export type MediaKind = "recording" | "source";

/** The engine's kind when it reports one; otherwise a file some pipeline
 * ingests is a source, and a name with "recording" in it is a recording. */
export function mediaKind(file: MediaFile): MediaKind {
  if (file.kind) return file.kind;
  if ((file.ingestCount ?? 0) > 0) return "source";
  return file.name.toLowerCase().includes("recording") ? "recording" : "source";
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
