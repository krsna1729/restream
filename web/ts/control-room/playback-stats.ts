// Playback stats for a preview: what this browser's player sees once a
// second (picture size, frames decoded and dropped, buffer, latency behind
// live, bandwidth), summarised for an overlay and exported as JSON so a
// bad preview can be attached to a ticket. Pure.

/** One reading of the player. Counters are cumulative, as the browser
 * reports them; a value the player does not report is null. */
export interface PlaybackSample {
  atMs: number;
  state: string;
  width: number;
  height: number;
  decodedFrames: number | null;
  droppedFrames: number | null;
  /** Seconds buffered ahead of the playhead. */
  bufferAheadSec: number;
  /** Seconds behind the live edge (hls.js), null with the native player. */
  latencySec: number | null;
  /** The player's bandwidth estimate, bits per second. */
  bandwidthBps: number | null;
}

/** A change worth reading in the export: a state change, a fallback, an
 * error. */
export interface PlaybackEvent {
  atMs: number;
  kind: string;
  detail?: string;
}

/** Samples kept: ten minutes at one a second. */
export const MAX_PLAYBACK_SAMPLES = 600;

export function appendSample(
  samples: readonly PlaybackSample[],
  sample: PlaybackSample,
): PlaybackSample[] {
  const next = [...samples, sample];
  return next.length > MAX_PLAYBACK_SAMPLES
    ? next.slice(next.length - MAX_PLAYBACK_SAMPLES)
    : next;
}

export interface PlaybackSummary {
  resolution: string | null;
  /** Frames decoded per second over the last two readings. */
  fps: number | null;
  /** Share of frames dropped since the preview started, 0..1. */
  droppedShare: number | null;
  bufferAheadSec: number | null;
  latencySec: number | null;
  bandwidthBps: number | null;
  /** Times playback stopped to buffer after it had played. */
  stalls: number;
}

export function summarizePlayback(
  samples: readonly PlaybackSample[],
): PlaybackSummary {
  const last = samples[samples.length - 1];
  const previous = samples[samples.length - 2];
  let fps: number | null = null;
  if (
    last &&
    previous &&
    last.decodedFrames !== null &&
    previous.decodedFrames !== null &&
    last.atMs > previous.atMs
  ) {
    // A counter that went backwards (a new media element) says nothing.
    const frames = last.decodedFrames - previous.decodedFrames;
    fps = frames >= 0 ? (frames * 1000) / (last.atMs - previous.atMs) : null;
  }
  const first = samples[0];
  let droppedShare: number | null = null;
  if (
    first &&
    last &&
    last.decodedFrames !== null &&
    last.droppedFrames !== null &&
    first.decodedFrames !== null &&
    first.droppedFrames !== null
  ) {
    const decoded = last.decodedFrames - first.decodedFrames;
    const dropped = last.droppedFrames - first.droppedFrames;
    droppedShare =
      decoded + dropped > 0 && dropped >= 0 ? dropped / (decoded + dropped) : 0;
  }
  let stalls = 0;
  let played = false;
  let state = "";
  for (const sample of samples) {
    if (sample.state === "playing") played = true;
    else if (sample.state === "buffering" && played && state !== "buffering")
      stalls += 1;
    state = sample.state;
  }
  return {
    resolution:
      last && last.width > 0 && last.height > 0
        ? `${last.width}×${last.height}`
        : null,
    fps,
    droppedShare,
    bufferAheadSec: last ? last.bufferAheadSec : null,
    latencySec: last?.latencySec ?? null,
    bandwidthBps: last?.bandwidthBps ?? null,
    stalls,
  };
}

/** The export: what played, where, and every reading and event. */
export function playbackReport(
  meta: { label: string; url: string; userAgent: string; exportedAtMs: number },
  samples: readonly PlaybackSample[],
  events: readonly PlaybackEvent[],
) {
  return {
    format: "restream.playback.v1",
    label: meta.label,
    url: meta.url,
    userAgent: meta.userAgent,
    exportedAt: new Date(meta.exportedAtMs).toISOString(),
    summary: summarizePlayback(samples),
    samples,
    events,
  };
}
