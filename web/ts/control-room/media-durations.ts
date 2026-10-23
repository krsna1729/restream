// The durations of media files, probed once per file by the engine
// (`/media/{name}/analysis`) and kept for the visit: a file's length does
// not change under its name.

import { useQueries } from "@tanstack/react-query";

import { getMediaFileAnalysis } from "../core/api.js";

/** One entry per name, in order: seconds, or null while unknown. */
export function useMediaDurations(names: readonly string[]): readonly (number | null)[] {
  return useQueries({
    queries: names.map((name) => ({
      queryKey: ["media-duration", name],
      queryFn: async () => (await getMediaFileAnalysis(name))?.durationSec ?? null,
      staleTime: Infinity,
      enabled: name.length > 0,
    })),
  }).map((query) => query.data ?? null);
}
