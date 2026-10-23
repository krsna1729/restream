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
