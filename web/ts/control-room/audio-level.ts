// Audio levels for the preview meter: dBFS from samples, and where a
// level sits on the meter. Pure.

/** The quietest level the meter draws, in dBFS. */
export const FLOOR_DB = -60;

/** RMS level of a block of samples in dBFS; FLOOR_DB for silence. */
export function levelDb(samples: Float32Array): number {
  let sum = 0;
  for (const sample of samples) sum += sample * sample;
  const rms = Math.sqrt(sum / Math.max(1, samples.length));
  return rms > 0 ? Math.max(FLOOR_DB, 20 * Math.log10(rms)) : FLOOR_DB;
}

/** Where a level sits on the meter, 0 (floor) to 1 (full scale). */
export function meterFraction(db: number): number {
  return Math.min(1, Math.max(0, (db - FLOOR_DB) / -FLOOR_DB));
}

/** Channel names in Web Audio order (`ChannelSplitterNode` outputs): mono,
 * stereo, quad, 5.1 and 7.1 are named; any other count is numbered. */
export function channelLabels(count: number): string[] {
  const named: Record<number, string[]> = {
    1: ["M"],
    2: ["L", "R"],
    4: ["L", "R", "Ls", "Rs"],
    6: ["L", "R", "C", "LFE", "Ls", "Rs"],
    8: ["L", "R", "C", "LFE", "Lb", "Rb", "Ls", "Rs"],
  };
  const whole = Math.max(1, Math.floor(count));
  return named[whole] ?? Array.from({ length: whole }, (_, index) => String(index + 1));
}

/** Channels an HLS rendition declares (`CHANNELS="6"`, or `"16/JOC"`),
 * clamped to what the meter draws; 2 when it says nothing usable. */
export function declaredChannels(channels: string | undefined, most = 8): number {
  const count = Number.parseInt(channels ?? "", 10);
  return Number.isFinite(count) && count > 0 ? Math.min(most, count) : 2;
}
