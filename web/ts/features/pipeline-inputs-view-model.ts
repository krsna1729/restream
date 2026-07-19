import type { PipelineInput } from "../types.js";

export function pipelineInputStatusLabel(input: PipelineInput): string {
  if (!input.enabled) return "Disabled";
  if (!input.runtime.connected) {
    return input.selected ? "Selected offline" : "Offline";
  }
  switch (input.runtime.forwardingState) {
    case "active":
      return "Forwarding";
    case "awaiting_keyframe":
      return "Awaiting keyframe";
    case "standby":
      return "Connected standby";
    default:
      return "Connected";
  }
}

export function formatPipelineInputBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes < 1024) return `${bytes.toFixed(0)} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) {
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

export function pipelineInputSubtitle(input: PipelineInput): string {
  const protocol = input.runtime.protocol?.toUpperCase() ?? "No publisher";
  const received = input.runtime.connected
    ? ` · ${formatPipelineInputBytes(input.runtime.bytesReceived)} received`
    : "";
  return `${input.selected ? "Selected" : "Standby"} · ${protocol}${received}`;
}
