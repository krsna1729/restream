import type { MediaFileAnalysis } from "../../core/api-types.js";
import type { PipelineView } from "../../types.js";

// ── File source helpers ────────────────────────────────────────────────

export function getFileSourceName(pipe: PipelineView): string | null {
  if (pipe.fileIngest?.filename) return pipe.fileIngest.filename;
  const inputSource = (pipe.inputSource || "").trim();
  if (!inputSource.startsWith("file:")) return null;
  const filename = inputSource.slice("file:".length).trim();
  return filename || null;
}

// ── File metadata formatters ───────────────────────────────────────────

export function formatFileSize(
  bytes: number | null | undefined,
): string {
  if (!Number.isFinite(bytes as number) || (bytes as number) <= 0) return "--";
  const value = bytes as number;
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
  if (value < 1024 * 1024 * 1024)
    return `${(value / (1024 * 1024)).toFixed(1)} MiB`;
  return `${(value / (1024 * 1024 * 1024)).toFixed(1)} GiB`;
}

export function formatFileModifiedAt(
  value: string | null | undefined,
): string {
  if (!value) return "--";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "--";
  return date.toLocaleString();
}

export function formatFileContainer(
  name: string | null | undefined,
): string {
  const ext = name?.split(".").pop()?.trim().toLowerCase() || "";
  switch (ext) {
    case "ts":
      return "MPEG-TS";
    case "mp4":
      return "MP4";
    case "mkv":
      return "Matroska";
    case "mov":
      return "QuickTime";
    default:
      return ext ? ext.toUpperCase() : "--";
  }
}

export function formatSourceDuration(
  value: number | null | undefined,
): string {
  if (!Number.isFinite(value as number) || (value as number) <= 0) return "--";
  return `${Number(value).toFixed(1)}s`;
}

export function formatSourceFps(
  value: number | null | undefined,
): string {
  if (!Number.isFinite(value as number) || (value as number) <= 0) return "--";
  const fps = Number(value);
  return `${fps.toFixed(fps === Math.round(fps) ? 0 : 1)} FPS`;
}

export function formatSourceGop(
  analysis: MediaFileAnalysis | null,
): string {
  if (
    !analysis ||
    !Number.isFinite(analysis.averageKeyframeIntervalSec as number) ||
    !Number.isFinite(analysis.maxKeyframeIntervalSec as number)
  ) {
    return "--";
  }
  return `avg ${Number(analysis.averageKeyframeIntervalSec).toFixed(1)}s | max ${Number(analysis.maxKeyframeIntervalSec).toFixed(1)}s`;
}
