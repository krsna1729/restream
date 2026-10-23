// Channels: every destination as a channel on a platform, the way an
// operator thinks of them ("our YouTube channels"), with bulk actions over
// a filtered set. The platform comes from the stored URL; the state from the
// runtime state stream.

import type { EgressProtocol, OutputState, RuntimeState } from "../core/generated/runtime-state.js";
import { isDelivering } from "./fleet-model.js";

export interface Platform {
  readonly id: string;
  readonly label: string;
  /** Two letters drawn in the channel's avatar. */
  readonly monogram: string;
}

const PLATFORMS: ReadonlyArray<Platform & { readonly hosts: readonly RegExp[] }> = [
  { id: "youtube", label: "YouTube", monogram: "YT", hosts: [/(^|\.)youtube\.com$/, /(^|\.)youtu\.be$/] },
  { id: "twitch", label: "Twitch", monogram: "TW", hosts: [/(^|\.)twitch\.tv$/, /(^|\.)contribute\.live-video\.net$/] },
  { id: "kick", label: "Kick", monogram: "KK", hosts: [/global-contribute\.live-video\.net$/, /(^|\.)kick\.com$/] },
  { id: "facebook", label: "Facebook", monogram: "FB", hosts: [/(^|\.)facebook\.com$/, /(^|\.)fbcdn\.net$/] },
  { id: "x", label: "X", monogram: "X", hosts: [/(^|\.)pscp\.tv$/, /(^|\.)x\.com$/, /(^|\.)twitter\.com$/] },
  { id: "linkedin", label: "LinkedIn", monogram: "IN", hosts: [/(^|\.)linkedin\.com$/] },
  { id: "instagram", label: "Instagram", monogram: "IG", hosts: [/(^|\.)instagram\.com$/] },
  { id: "vimeo", label: "Vimeo", monogram: "VI", hosts: [/(^|\.)vimeo\.com$/] },
];

const CUSTOM: Record<EgressProtocol, Platform> = {
  rtmp: { id: "custom-rtmp", label: "Custom RTMP", monogram: "RT" },
  srt: { id: "custom-srt", label: "Custom SRT", monogram: "SR" },
  hls: { id: "custom-hls", label: "HLS push", monogram: "HL" },
  moq: { id: "moq", label: "MoQ relay", monogram: "MQ" },
  pipeline: { id: "pipeline", label: "Another pipeline", monogram: "PL" },
  sink: { id: "sink", label: "Sink", monogram: "SK" },
  unknown: { id: "unknown", label: "Unknown", monogram: "??" },
};

/** The platform a destination URL sends to; a host no platform claims is a
 * custom destination of its protocol. Kick's ingest is checked before
 * Twitch's because both are served from live-video.net. */
export function platformOf(url: string | null | undefined, protocol: EgressProtocol): Platform {
  let host = "";
  try {
    host = url ? new URL(url).hostname.toLowerCase() : "";
  } catch {
    host = "";
  }
  if (host) {
    const kick = PLATFORMS.find((platform) => platform.id === "kick");
    if (kick && kick.hosts.some((pattern) => pattern.test(host))) return strip(kick);
    for (const platform of PLATFORMS) {
      if (platform.hosts.some((pattern) => pattern.test(host))) return strip(platform);
    }
  }
  return CUSTOM[protocol];
}

function strip(platform: Platform): Platform {
  return { id: platform.id, label: platform.label, monogram: platform.monogram };
}

export type ChannelState = "live" | "starting" | "retrying" | "failed" | "degraded" | "waiting" | "off";

export interface Channel {
  readonly output: OutputState;
  readonly pipelineName: string;
  readonly platform: Platform;
  readonly state: ChannelState;
}

export function channelState(output: OutputState): ChannelState {
  if (output.desired === "stopped") return "off";
  if (output.convergence === "retriesExhausted") return "failed";
  if (output.convergence === "retrying") return "retrying";
  if (output.convergence === "waitingForIngest") return "waiting";
  if (output.diagnosis) return "degraded";
  if (isDelivering(output)) return "live";
  return "starting";
}

/** Every output as a channel, with its platform from the stored URL. */
export function buildChannels(state: RuntimeState, urls: ReadonlyMap<string, string>): Channel[] {
  const names = new Map(state.pipelines.map((pipeline) => [pipeline.id, pipeline.name]));
  return state.outputs.map((output) => ({
    output,
    pipelineName: names.get(output.pipelineId) ?? output.pipelineId,
    platform: platformOf(urls.get(output.id), output.protocol),
    state: channelState(output),
  }));
}

export interface PlatformSummary {
  readonly platform: Platform;
  readonly total: number;
  readonly live: number;
  /** Wanted but not live: retrying, failed, degraded, waiting, starting. */
  readonly troubled: number;
}

/** One row per platform, the most channels first. */
export function platformSummaries(channels: readonly Channel[]): PlatformSummary[] {
  const byId = new Map<string, { platform: Platform; total: number; live: number; troubled: number }>();
  for (const channel of channels) {
    const entry = byId.get(channel.platform.id) ?? { platform: channel.platform, total: 0, live: 0, troubled: 0 };
    entry.total += 1;
    if (channel.state === "live") entry.live += 1;
    else if (channel.state !== "off") entry.troubled += 1;
    byId.set(channel.platform.id, entry);
  }
  return [...byId.values()].sort((a, b) => b.total - a.total || a.platform.label.localeCompare(b.platform.label));
}

export type BulkAction = "start" | "stop" | "retry";

/** The channels a bulk action would touch: start the ones off, stop the
 * ones wanted, retry the ones retrying or given up (a stop then a start,
 * which resets the engine's retry budget). */
export function bulkTargets(channels: readonly Channel[], action: BulkAction): Channel[] {
  switch (action) {
    case "start":
      return channels.filter((channel) => channel.state === "off");
    case "stop":
      return channels.filter((channel) => channel.state !== "off");
    case "retry":
      return channels.filter((channel) => channel.state === "retrying" || channel.state === "failed");
  }
}

/**
 * Runs `act` over `items` with at most `concurrency` in flight, reporting
 * progress after each. A failed item is counted, never thrown: one refused
 * destination must not abandon the rest of a bulk start.
 */
export async function runBulk<T>(
  items: readonly T[],
  act: (item: T) => Promise<boolean>,
  concurrency: number,
  onProgress: (done: number, failed: number) => void,
): Promise<{ done: number; failed: number }> {
  let next = 0;
  let done = 0;
  let failed = 0;
  const worker = async () => {
    while (next < items.length) {
      const item = items[next];
      next += 1;
      let ok = false;
      try {
        ok = await act(item);
      } catch {
        ok = false;
      }
      done += 1;
      if (!ok) failed += 1;
      onProgress(done, failed);
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(concurrency, items.length)) }, worker));
  return { done, failed };
}
