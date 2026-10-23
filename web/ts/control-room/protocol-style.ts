// One colour per egress protocol, used wherever the console shows a mix.
// Protocol is a category, not a status, so these avoid the status hues.

import type { EgressProtocol } from "../core/generated/runtime-state.js";

export const PROTOCOL_COLOR: Record<EgressProtocol, string> = {
  rtmp: "bg-primary",
  srt: "bg-secondary",
  hls: "bg-accent",
  moq: "bg-info",
  pipeline: "bg-neutral",
  sink: "bg-base-content/40",
  unknown: "bg-base-content/25",
};

export const PROTOCOL_LABEL: Record<EgressProtocol, string> = {
  rtmp: "RTMP",
  srt: "SRT",
  hls: "HLS",
  moq: "MoQ",
  pipeline: "Pipeline",
  sink: "Sink",
  unknown: "Unknown",
};
